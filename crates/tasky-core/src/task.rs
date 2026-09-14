use crate::{Error, LinkKind, Result, TaskStatus, Timestamp, require_nonblank};
use serde::{Deserialize, Serialize};

/// The basic unit of work. Belongs to a goal and may depend on tasks anywhere in its project.
///
/// Lifecycle: `todo → in_progress ⇄ testing → ready_for_merge → done`, with `cancelled`
/// reachable from any non-terminal state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub goal_id: String,
    pub title: String,
    pub body: String,
    /// The validation steps that prove the task complete. Free text, may be empty.
    pub test_plan: String,
    /// Link to the pull request that delivers the task, once one exists.
    pub pr: Option<String>,
    pub status: TaskStatus,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub completed_at: Option<Timestamp>,
}

/// `task_id` requires `depends_on_id` to be done first.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Dependency {
    pub task_id: String,
    pub depends_on_id: String,
}

/// An external reference attached to a task: a commit or a URL. The pull request that
/// delivers a task is a field on the task itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLink {
    pub id: String,
    pub task_id: String,
    pub kind: LinkKind,
    pub reference: String,
    pub created_at: Timestamp,
}

impl Task {
    /// Create a todo task.
    ///
    /// # Errors
    /// Returns an error if an ID or the title is blank.
    pub fn new(
        id: String,
        goal_id: String,
        title: String,
        body: String,
        test_plan: String,
        now: Timestamp,
    ) -> Result<Self> {
        require_nonblank("id", &id)?;
        require_nonblank("goal id", &goal_id)?;
        require_nonblank("title", &title)?;
        Ok(Self {
            id,
            goal_id,
            title,
            body,
            test_plan,
            pr: None,
            status: TaskStatus::Todo,
            created_at: now,
            updated_at: now,
            completed_at: None,
        })
    }

    /// Replace the test plan of an open task.
    ///
    /// # Errors
    /// Returns an error if the task is done or cancelled.
    pub fn set_test_plan(&mut self, test_plan: String, now: Timestamp) -> Result<()> {
        self.require_open("given a test plan")?;
        self.test_plan = test_plan;
        self.updated_at = now;
        Ok(())
    }

    /// Set or clear the pull request link of an open task. Blank text clears it.
    ///
    /// # Errors
    /// Returns an error if the task is done or cancelled.
    pub fn set_pr(&mut self, pr: Option<String>, now: Timestamp) -> Result<()> {
        self.require_open("given a pull request")?;
        self.pr = pr.filter(|text| !text.trim().is_empty());
        self.updated_at = now;
        Ok(())
    }

    /// Begin work on a todo task whose dependencies are all done.
    ///
    /// # Errors
    /// Returns an error if the task is not todo or a dependency is unfinished.
    pub fn start(&mut self, dependencies_done: bool, now: Timestamp) -> Result<()> {
        self.require_status(TaskStatus::Todo, "started")?;
        if !dependencies_done {
            return Err(Error::Invalid(format!(
                "task {} has unfinished dependencies",
                self.id
            )));
        }
        self.transition(TaskStatus::InProgress, now);
        Ok(())
    }

    /// Hand an in-progress task over to validation against its test plan.
    ///
    /// # Errors
    /// Returns an error if the task is not in progress.
    pub fn begin_testing(&mut self, now: Timestamp) -> Result<()> {
        self.require_status(TaskStatus::InProgress, "tested")?;
        self.transition(TaskStatus::Testing, now);
        Ok(())
    }

    /// Send a task that failed validation back to in progress.
    ///
    /// # Errors
    /// Returns an error if the task is not testing.
    pub fn fail_testing(&mut self, now: Timestamp) -> Result<()> {
        self.require_status(TaskStatus::Testing, "failed")?;
        self.transition(TaskStatus::InProgress, now);
        Ok(())
    }

    /// Record that validation passed; the task now waits to be merged.
    ///
    /// # Errors
    /// Returns an error if the task is not testing.
    pub fn pass_testing(&mut self, now: Timestamp) -> Result<()> {
        self.require_status(TaskStatus::Testing, "passed")?;
        self.transition(TaskStatus::ReadyForMerge, now);
        Ok(())
    }

    /// Mark a merged task done. Only a task that is ready for merge can finish.
    ///
    /// # Errors
    /// Returns an error if the task is not ready for merge.
    pub fn finish(&mut self, now: Timestamp) -> Result<()> {
        self.require_status(TaskStatus::ReadyForMerge, "finished")?;
        self.transition(TaskStatus::Done, now);
        self.completed_at = Some(now);
        Ok(())
    }

    /// Cancel an open task. Tasks that depend on it stay blocked until they drop the dependency.
    ///
    /// # Errors
    /// Returns an error if the task is already done or cancelled.
    pub fn cancel(&mut self, now: Timestamp) -> Result<()> {
        self.require_open("cancelled")?;
        self.transition(TaskStatus::Cancelled, now);
        Ok(())
    }

    /// Dependencies may only change while a task has not started.
    ///
    /// # Errors
    /// Returns an error if the task is not todo.
    pub fn require_editable_dependencies(&self) -> Result<()> {
        self.require_status(TaskStatus::Todo, "given new dependencies")
    }

    fn transition(&mut self, status: TaskStatus, now: Timestamp) {
        self.status = status;
        self.updated_at = now;
    }

    fn require_status(&self, expected: TaskStatus, verb: &str) -> Result<()> {
        if self.status != expected {
            return Err(Error::Invalid(format!(
                "task {} is {} and cannot be {verb}",
                self.id, self.status
            )));
        }
        Ok(())
    }

    fn require_open(&self, verb: &str) -> Result<()> {
        if !self.status.is_open() {
            return Err(Error::Invalid(format!(
                "task {} is {} and cannot be {verb}",
                self.id, self.status
            )));
        }
        Ok(())
    }
}

impl Dependency {
    /// Describe an edge. Self-dependencies are rejected here; cycles are checked by [`crate::Dag`].
    ///
    /// # Errors
    /// Returns an error if either ID is blank or both are the same task.
    pub fn new(task_id: String, depends_on_id: String) -> Result<Self> {
        require_nonblank("task id", &task_id)?;
        require_nonblank("dependency id", &depends_on_id)?;
        if task_id == depends_on_id {
            return Err(Error::Invalid("a task cannot depend on itself".into()));
        }
        Ok(Self {
            task_id,
            depends_on_id,
        })
    }
}

impl TaskLink {
    /// Attach an external reference to a task.
    ///
    /// # Errors
    /// Returns an error if an ID or the reference is blank.
    pub fn new(
        id: String,
        task_id: String,
        kind: LinkKind,
        reference: String,
        now: Timestamp,
    ) -> Result<Self> {
        require_nonblank("id", &id)?;
        require_nonblank("task id", &task_id)?;
        require_nonblank("reference", &reference)?;
        Ok(Self {
            id,
            task_id,
            kind,
            reference,
            created_at: now,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Timestamp = Timestamp::UNIX_EPOCH;

    fn task() -> Task {
        Task::new(
            "T".into(),
            "F".into(),
            "Work".into(),
            String::new(),
            String::new(),
            NOW,
        )
        .unwrap()
    }

    #[test]
    fn happy_path_walks_every_state_in_order() {
        let mut task = task();
        assert!(task.start(false, NOW).is_err(), "blocked");
        task.start(true, NOW).unwrap();
        assert_eq!(task.status, TaskStatus::InProgress);
        task.begin_testing(NOW).unwrap();
        assert_eq!(task.status, TaskStatus::Testing);
        task.fail_testing(NOW).unwrap();
        assert_eq!(task.status, TaskStatus::InProgress);
        task.begin_testing(NOW).unwrap();
        task.pass_testing(NOW).unwrap();
        assert_eq!(task.status, TaskStatus::ReadyForMerge);
        task.finish(NOW).unwrap();
        assert_eq!(task.status, TaskStatus::Done);
        assert!(task.completed_at.is_some());
        assert!(!task.status.is_open());
    }

    #[test]
    fn no_state_can_be_skipped_or_revisited_after_done() {
        let mut task = task();
        assert!(task.finish(NOW).is_err(), "todo cannot finish");
        assert!(task.begin_testing(NOW).is_err(), "todo cannot test");
        assert!(task.pass_testing(NOW).is_err());
        assert!(task.fail_testing(NOW).is_err());
        task.start(true, NOW).unwrap();
        assert!(task.start(true, NOW).is_err());
        assert!(task.finish(NOW).is_err(), "in progress cannot finish");
        assert!(task.pass_testing(NOW).is_err(), "must be testing first");
        assert!(task.require_editable_dependencies().is_err());
        task.begin_testing(NOW).unwrap();
        assert!(task.finish(NOW).is_err(), "testing cannot finish");
        task.pass_testing(NOW).unwrap();
        assert!(
            task.fail_testing(NOW).is_err(),
            "ready for merge cannot fail"
        );
        task.finish(NOW).unwrap();
        assert!(task.cancel(NOW).is_err());
        assert!(task.finish(NOW).is_err());
        assert!(task.set_pr(Some("x".into()), NOW).is_err());
        assert!(task.set_test_plan("x".into(), NOW).is_err());
    }

    #[test]
    fn cancel_works_from_every_open_state() {
        for steps in 0..4 {
            let mut task = task();
            let moves: [fn(&mut Task) -> Result<()>; 4] = [
                |t| t.start(true, NOW),
                |t| t.begin_testing(NOW),
                |t| t.pass_testing(NOW),
                |t| t.finish(NOW),
            ];
            for step in moves.iter().take(steps) {
                step(&mut task).unwrap();
            }
            task.cancel(NOW).unwrap();
            assert_eq!(task.status, TaskStatus::Cancelled, "after {steps} steps");
            assert!(task.start(true, NOW).is_err());
        }
    }

    #[test]
    fn test_plan_and_pr_are_editable_while_open() {
        let mut task = task();
        task.set_test_plan("1. run cargo test".into(), NOW).unwrap();
        assert_eq!(task.test_plan, "1. run cargo test");
        task.set_pr(Some("https://example.com/pr/1".into()), NOW)
            .unwrap();
        assert_eq!(task.pr.as_deref(), Some("https://example.com/pr/1"));
        task.set_pr(Some("  ".into()), NOW).unwrap();
        assert_eq!(task.pr, None, "blank clears");
        task.cancel(NOW).unwrap();
        assert!(task.set_pr(Some("x".into()), NOW).is_err());
    }

    #[test]
    fn links_and_edges_are_validated() {
        assert!(Dependency::new("a".into(), "a".into()).is_err());
        assert!(
            TaskLink::new(
                "L".into(),
                "T".into(),
                LinkKind::Commit,
                "abc123".into(),
                NOW
            )
            .is_ok()
        );
        assert!(TaskLink::new("L".into(), "T".into(), LinkKind::Url, " ".into(), NOW).is_err());
    }
}
