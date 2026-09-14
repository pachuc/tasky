use serde_json::Value;
use std::{
    path::Path,
    process::{Command, Output},
};

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tasky"))
        .arg("--db")
        .arg(dir.join("tasky.db"))
        .arg("--json")
        .args(args)
        .output()
        .unwrap()
}

fn ok(dir: &Path, args: &[&str]) -> Value {
    let output = run(dir, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn fails(dir: &Path, args: &[&str]) -> String {
    let output = run(dir, args);
    assert_eq!(output.status.code(), Some(1), "{args:?} should fail");
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    error["error"]["message"].as_str().unwrap().to_owned()
}

fn id(value: &Value) -> String {
    value["id"].as_str().unwrap().to_owned()
}

fn len(value: &Value) -> usize {
    value.as_array().unwrap().len()
}

/// A database with projects `app` and `web`, each holding a goal `auth`; `app/auth` is active.
fn setup(p: &Path) {
    ok(p, &["init"]);
    ok(p, &["project", "add", "app"]);
    ok(p, &["project", "add", "web", "Website"]);
    ok(p, &["goal", "add", "app", "auth", "Authentication"]);
    ok(p, &["goal", "add", "web", "auth", "Web auth"]);
    ok(p, &["goal", "activate", "app/auth"]);
}

#[test]
fn projects_and_goals_nest() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    setup(p);
    let mobile = ok(
        p,
        &["project", "add", "mobile", "Mobile", "--parent", "app"],
    );
    assert!(mobile["parent_id"].is_string());
    ok(p, &["project", "add", "ios", "--parent", "app/mobile"]);
    fails(p, &["project", "add", "mobile", "--parent", "app"]);
    fails(p, &["project", "add", "x", "--parent", "nope"]);
    let shown = ok(p, &["project", "show", "app/mobile/ios"]);
    assert_eq!(shown["path"], "app/mobile/ios");
    assert_eq!(ok(p, &["project", "show", "app"])["subprojects"], 1);

    let login = ok(
        p,
        &[
            "goal", "add", "app", "login", "Login", "--parent", "app/auth",
        ],
    );
    assert!(login["parent_id"].is_string());
    fails(p, &["goal", "add", "app", "x", "X", "--parent", "web/auth"]);
    ok(p, &["goal", "activate", "app/login"]);
    let task = id(&ok(p, &["task", "add", "app/login", "Build login"]));
    assert_eq!(ok(p, &["goal", "show", "app/auth"])["subgoals"], 1);
    assert_eq!(ok(p, &["goal", "show", "app/auth"])["tasks"]["todo"], 1);
    assert!(fails(p, &["goal", "complete", "app/auth"]).contains("sub-goal"));
    for step in ["start", "test", "pass", "done"] {
        ok(p, &["task", step, &task]);
    }
    ok(p, &["goal", "complete", "app/login"]);
    assert_eq!(
        ok(p, &["goal", "complete", "app/auth"])["status"],
        "complete"
    );

    ok(p, &["goal", "add", "app/mobile", "shell", "App shell"]);
    assert_eq!(len(&ok(p, &["goal", "list", "--project", "app"])), 3);
    assert_eq!(len(&ok(p, &["goal", "list", "--project", "app/mobile"])), 1);
    let shell = id(&ok(p, &["task", "add", "app/mobile/shell", "Scaffold"]));
    let web = id(&ok(p, &["task", "add", "web/auth", "Web task"]));
    ok(p, &["task", "depend", &shell, &task]);
    fails(p, &["task", "depend", &web, &shell]);
    assert_eq!(len(&ok(p, &["task", "list", "--project", "app"])), 2);
    assert_eq!(ok(p, &["task", "show", &shell])["project"], "app/mobile");
}

#[test]
fn projects_carry_optional_repo_path_and_url() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    ok(p, &["init"]);
    let app = ok(p, &["project", "add", "app"]);
    assert_eq!(app["name"], "app");
    assert_eq!(app["repo_path"], Value::Null);
    assert_eq!(app["repo_url"], Value::Null);
    let web = ok(
        p,
        &[
            "project",
            "add",
            "web",
            "Website",
            "--repo-url",
            "https://example.com/web.git",
            "--repo-path",
            "/home/me/web",
        ],
    );
    assert_eq!(web["name"], "Website");
    assert_eq!(web["repo_url"], "https://example.com/web.git");
    assert_eq!(web["repo_path"], "/home/me/web");
    fails(p, &["project", "add", "app"]);
    assert_eq!(len(&ok(p, &["project", "list"])), 2);
    assert_eq!(ok(p, &["project", "show", "app"])["goals"], 0);
    let tied = ok(p, &["project", "repo", "app", "--path", "/home/me/app"]);
    assert_eq!(tied["repo_path"], "/home/me/app");
    assert_eq!(tied["repo_url"], Value::Null);
    let both = ok(
        p,
        &["project", "repo", "app", "--url", "git@example.com:app.git"],
    );
    assert_eq!(both["repo_path"], "/home/me/app", "path kept");
    assert_eq!(both["repo_url"], "git@example.com:app.git");
    let cleared = ok(
        p,
        &["project", "repo", "app", "--clear-path", "--clear-url"],
    );
    assert_eq!(cleared["repo_path"], Value::Null);
    assert_eq!(cleared["repo_url"], Value::Null);
    assert_eq!(run(p, &["project", "repo", "app"]).status.code(), Some(2));
    assert_eq!(
        run(
            p,
            &["project", "repo", "app", "--path", "x", "--clear-path"]
        )
        .status
        .code(),
        Some(2)
    );
}

#[test]
fn projects_and_goals_are_managed_through_the_cli() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    assert!(ok(p, &["init"])["created"].is_string());
    fails(p, &["init"]);
    assert!(
        ok(p, &["migrate"])["applied"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    ok(p, &["project", "add", "app"]);
    ok(p, &["project", "add", "web", "Website"]);

    let goal = ok(
        p,
        &[
            "goal",
            "add",
            "app",
            "auth",
            "Authentication",
            "--spec",
            "# Auth\n\nUsers can log in.\n",
        ],
    );
    assert_eq!(goal["status"], "draft");
    assert!(goal["spec"].as_str().unwrap().contains("log in"));
    fails(p, &["goal", "add", "app", "auth", "Again"]);
    ok(p, &["goal", "add", "web", "auth", "Web auth"]);
    assert_eq!(len(&ok(p, &["goal", "list"])), 2);
    assert_eq!(len(&ok(p, &["goal", "list", "--project", "web"])), 1);
    assert!(fails(p, &["goal", "show", "auth"]).contains("PROJECT/auth"));
    assert_eq!(ok(p, &["goal", "show", "app/auth"])["project"], "app");
    assert_eq!(ok(p, &["goal", "show", "web/auth"])["spec"], Value::Null);

    let spec_file = p.join("spec.md");
    std::fs::write(&spec_file, "# Web auth\n").unwrap();
    let written = ok(
        p,
        &[
            "goal",
            "spec",
            "web/auth",
            "--file",
            spec_file.to_str().unwrap(),
        ],
    );
    assert_eq!(written["spec"], "# Web auth\n");
    assert_eq!(
        ok(p, &["goal", "spec", "web/auth", "--clear"])["spec"],
        Value::Null
    );
    assert_eq!(ok(p, &["goal", "activate", "app/auth"])["status"], "active");
    fails(p, &["goal", "activate", "app/auth"]);
    fails(p, &["goal", "complete", "web/auth"]);
    assert_eq!(
        ok(p, &["goal", "cancel", "web/auth"])["status"],
        "cancelled"
    );
    fails(p, &["goal", "spec", "web/auth", "--text", "late"]);
}

#[test]
fn tasks_form_a_gated_graph_within_a_project() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    setup(p);

    let design = id(&ok(p, &["task", "add", "app/auth", "Design API"]));
    let build = id(&ok(
        p,
        &["task", "add", "app/auth", "Build API", "--body", "Rust"],
    ));
    let web_task = id(&ok(p, &["task", "add", "web/auth", "Web login"]));
    ok(p, &["task", "depend", &build, &design]);
    fails(p, &["task", "depend", &design, &build]);
    assert!(fails(p, &["task", "depend", &web_task, &design]).contains("different projects"));
    assert_eq!(len(&ok(p, &["task", "ready"])), 2);
    assert_eq!(len(&ok(p, &["task", "ready", "--project", "app"])), 1);
    assert_eq!(len(&ok(p, &["task", "list", "--goal", "app/auth"])), 2);
    assert_eq!(len(&ok(p, &["task", "list", "--project", "web"])), 1);
    fails(
        p,
        &["task", "list", "--project", "web", "--goal", "app/auth"],
    );
    let order = ok(p, &["task", "order", "--project", "app"]);
    assert_eq!(order[0]["id"], design);
    assert_eq!(order[1]["id"], build);
    fails(p, &["task", "start", &build]);

    let short = &design[design.len() - 6..];
    assert_eq!(ok(p, &["task", "start", short])["status"], "in_progress");
    fails(p, &["task", "done", &design]);
    fails(p, &["task", "pass", &design]);
    assert_eq!(ok(p, &["task", "test", &design])["status"], "testing");
    assert_eq!(ok(p, &["task", "fail", &design])["status"], "in_progress");
    ok(p, &["task", "test", &design]);
    assert_eq!(
        ok(p, &["task", "pass", &design])["status"],
        "ready_for_merge"
    );
    assert_eq!(len(&ok(p, &["task", "ready"])), 1, "build still blocked");
    assert_eq!(
        ok(p, &["task", "list", "--status", "ready_for_merge"])[0]["id"],
        design
    );
    assert_eq!(ok(p, &["task", "done", &design])["status"], "done");
    let detail = ok(p, &["task", "show", &build]);
    assert_eq!(detail["project"], "app");
    assert_eq!(detail["goal"], "auth");
    assert_eq!(detail["ready"], true);
    assert_eq!(detail["depends_on"][0], design);
    assert!(detail["blocked_by"].as_array().unwrap().is_empty());

    ok(p, &["task", "link", &build, "--commit", "abc123"]);
    assert_eq!(
        ok(p, &["task", "show", &build])["links"][0]["kind"],
        "commit"
    );
    fails(p, &["goal", "complete", "app/auth"]);
    ok(p, &["task", "start", &build]);
    ok(p, &["task", "test", &build]);
    ok(p, &["task", "pass", &build]);
    ok(p, &["task", "done", &build]);
    assert_eq!(len(&ok(p, &["task", "list", "--status", "done"])), 2);
    let complete = ok(p, &["goal", "complete", "app/auth"]);
    assert_eq!(complete["status"], "complete");
    assert!(complete["completed_at"].is_string());
    assert_eq!(ok(p, &["goal", "show", "app/auth"])["tasks"]["done"], 2);
    assert_eq!(ok(p, &["project", "show", "app"])["tasks"]["done"], 2);
}

#[test]
fn title_body_test_plan_and_pr_are_fields_on_the_task() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    setup(p);
    let plan_file = p.join("plan.md");
    std::fs::write(&plan_file, "1. cargo test\n").unwrap();
    let body_file = p.join("body.md");
    std::fs::write(&body_file, "Build it.\n").unwrap();
    let task = ok(
        p,
        &[
            "task",
            "add",
            "app/auth",
            "Work",
            "--body-file",
            body_file.to_str().unwrap(),
            "--test-plan-file",
            plan_file.to_str().unwrap(),
        ],
    );
    assert_eq!(task["body"], "Build it.\n");
    assert_eq!(task["test_plan"], "1. cargo test\n");
    assert_eq!(task["pr"], Value::Null);
    let id = id(&task);
    assert_eq!(
        ok(p, &["task", "title", &id, "Work well"])["title"],
        "Work well"
    );
    assert_eq!(run(p, &["task", "title", &id, " "]).status.code(), Some(1));
    assert_eq!(
        ok(p, &["task", "body", &id, "--text", "Build it well."])["body"],
        "Build it well."
    );
    assert_eq!(
        ok(p, &["task", "test-plan", &id, "--text", "2. clippy"])["test_plan"],
        "2. clippy"
    );
    assert_eq!(
        ok(p, &["task", "pr", &id, "https://example.com/pr/9"])["pr"],
        "https://example.com/pr/9"
    );
    assert_eq!(ok(p, &["task", "pr", &id, "--clear"])["pr"], Value::Null);
    assert_eq!(run(p, &["task", "pr", &id]).status.code(), Some(2));
    ok(p, &["task", "pr", &id, "#9"]);
    assert_eq!(ok(p, &["task", "cancel", &id])["status"], "cancelled");
    fails(p, &["task", "pr", &id, "late"]);
    assert_eq!(ok(p, &["task", "show", &id])["pr"], "#9");
}

#[test]
fn competing_processes_cannot_both_start_the_same_task() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    setup(p);
    let task = id(&ok(p, &["task", "add", "app/auth", "Task"]));
    let results = std::thread::scope(|scope| {
        let first = scope.spawn(|| run(p, &["task", "start", &task]));
        let second = scope.spawn(|| run(p, &["task", "start", &task]));
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|r| r.status.success()).count(), 1);
    assert_eq!(ok(p, &["task", "show", &task])["status"], "in_progress");
}
