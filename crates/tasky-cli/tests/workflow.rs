use serde_json::Value;
use std::process::{Command, Output};

fn run(dir: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tasky"))
        .arg("--store")
        .arg(dir)
        .arg("--json")
        .args(args)
        .output()
        .unwrap()
}

fn ok(dir: &std::path::Path, args: &[&str]) -> Value {
    let output = run(dir, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn agent_workflow_persists_and_exposes_every_action() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    ok(p, &["init"]);
    ok(p, &["add", "design", "Design API"]);
    ok(p, &["add", "build", "Build API"]);
    ok(p, &["depend", "build", "design"]);
    assert_eq!(ok(p, &["ready"]).as_array().unwrap().len(), 1);
    assert_eq!(ok(p, &["list"]).as_array().unwrap().len(), 2);
    ok(p, &["undepend", "build", "design"]);
    assert_eq!(ok(p, &["ready"]).as_array().unwrap().len(), 2);
    ok(p, &["depend", "build", "design"]);
    let rejected = run(p, &["claim", "build", "--agent", "a"]);
    assert_eq!(rejected.status.code(), Some(1));
    assert!(rejected.stdout.is_empty());
    assert!(
        serde_json::from_slice::<Value>(&rejected.stderr).unwrap()["error"]["message"].is_string()
    );
    ok(p, &["claim", "design", "--agent", "a"]);
    ok(
        p,
        &["fail", "design", "--agent", "a", "--reason", "try again"],
    );
    ok(p, &["retry", "design"]);
    ok(p, &["claim", "design", "--agent", "b"]);
    ok(p, &["complete", "design", "--agent", "b"]);
    assert_eq!(ok(p, &["ready"])[0]["id"], "build");
    assert_eq!(ok(p, &["show", "design"])["status"]["state"], "done");
    assert_eq!(ok(p, &["snapshot"])["schema_version"], 1);
    assert_eq!(ok(p, &["validate"])["valid"], true);
}

#[test]
fn competing_processes_cannot_both_claim_the_same_task() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    ok(p, &["init"]);
    ok(p, &["add", "a", "Task"]);
    let results = std::thread::scope(|scope| {
        let first = scope.spawn(|| run(p, &["claim", "a", "--agent", "first"]));
        let second = scope.spawn(|| run(p, &["claim", "a", "--agent", "second"]));
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|r| r.status.success()).count(), 1);
    assert_eq!(ok(p, &["show", "a"])["status"]["state"], "running");
}
