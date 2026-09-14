use crate::{Error, GoalStatus, Result, Task, Timestamp, require_nonblank, validate_slug};
use serde::{Deserialize, Serialize};

/// A high-level goal within a project. Tasks attach directly to it; a spec is optional.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    pub id: String,
    pub project_id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    /// A detailed specification of the goal, when one has been written.
    pub spec: Option<String>,
    pub status: GoalStatus,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub completed_at: Option<Timestamp>,
}

/// Blank text means "no spec", so callers cannot store whitespace by accident.
fn normalize_spec(spec: Option<String>) -> Option<String> {
    spec.filter(|text| !text.trim().is_empty())
}

impl Goal {
    /// Create a draft goal.
    ///
    /// # Errors
    /// Returns an error if an ID is blank, the slug is malformed, or the title is blank.
    pub fn new(
        id: String,
        project_id: String,
        slug: String,
        title: String,
        description: String,
        spec: Option<String>,
        now: Timestamp,
    ) -> Result<Self> {
        require_nonblank("id", &id)?;
        require_nonblank("project id", &project_id)?;
        validate_slug(&slug)?;
        require_nonblank("title", &title)?;
        Ok(Self {
            id,
            project_id,
            slug,
            title,
            description,
            spec: normalize_spec(spec),
            status: GoalStatus::Draft,
            created_at: now,
            updated_at: now,
            completed_at: None,
        })
    }

    /// Whether the goal can still change: gain tasks, a new spec, or a new status.
    ///
    /// # Errors
    /// Returns an error if the goal is complete or cancelled.
    pub fn require_open(&self) -> Result<()> {
        match self.status {
            GoalStatus::Draft | GoalStatus::Active => Ok(()),
            status => Err(Error::Invalid(format!(
                "goal {} is {status} and cannot change",
                self.slug
            ))),
        }
    }

    /// Replace or clear the spec of an open goal.
    ///
    /// # Errors
    /// Returns an error if the goal is complete or cancelled.
    pub fn set_spec(&mut self, spec: Option<String>, now: Timestamp) -> Result<()> {
        self.require_open()?;
        self.spec = normalize_spec(spec);
        self.updated_at = now;
        Ok(())
    }

    /// Move a draft goal into active work.
    ///
    /// # Errors
    /// Returns an error if the goal is not draft.
    pub fn activate(&mut self, now: Timestamp) -> Result<()> {
        if self.status != GoalStatus::Draft {
            return Err(Error::Invalid(format!(
                "goal {} is {} and cannot be activated",
                self.slug, self.status
            )));
        }
        self.status = GoalStatus::Active;
        self.updated_at = now;
        Ok(())
    }

    /// Complete an active goal whose tasks are all finished.
    ///
    /// # Errors
    /// Returns an error if the goal is not active or a task is still open.
    pub fn complete(&mut self, tasks: &[Task], now: Timestamp) -> Result<()> {
        if self.status != GoalStatus::Active {
            return Err(Error::Invalid(format!(
                "goal {} is {} and cannot be completed",
                self.slug, self.status
            )));
        }
        if let Some(task) = tasks.iter().find(|task| task.status.is_open()) {
            return Err(Error::Invalid(format!(
                "task {} ({}) is still {}",
                task.id, task.title, task.status
            )));
        }
        self.status = GoalStatus::Complete;
        self.updated_at = now;
        self.completed_at = Some(now);
        Ok(())
    }

    /// Cancel a draft or active goal.
    ///
    /// # Errors
    /// Returns an error if the goal is already complete or cancelled.
    pub fn cancel(&mut self, now: Timestamp) -> Result<()> {
        self.require_open()?;
        self.status = GoalStatus::Cancelled;
        self.updated_at = now;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TaskStatus;

    fn now() -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn goal() -> Goal {
        Goal::new(
            "F1".into(),
            "P1".into(),
            "auth".into(),
            "Auth".into(),
            String::new(),
            None,
            now(),
        )
        .unwrap()
    }

    #[test]
    fn creation_validates_fields() {
        assert!(
            Goal::new(
                "F".into(),
                "P".into(),
                "Bad Slug".into(),
                "T".into(),
                String::new(),
                None,
                now()
            )
            .is_err()
        );
        assert!(
            Goal::new(
                "F".into(),
                "P".into(),
                "ok".into(),
                " ".into(),
                String::new(),
                None,
                now()
            )
            .is_err()
        );
        let with_spec = Goal::new(
            "F".into(),
            "P".into(),
            "ok".into(),
            "T".into(),
            String::new(),
            Some("  ".into()),
            now(),
        )
        .unwrap();
        assert_eq!(with_spec.spec, None, "blank specs are dropped");
    }

    #[test]
    fn spec_is_optional_and_editable_while_open() {
        let mut goal = goal();
        assert_eq!(goal.spec, None);
        goal.activate(now()).unwrap();
        assert!(goal.activate(now()).is_err());
        goal.set_spec(Some("Requirements".into()), now()).unwrap();
        assert_eq!(goal.spec.as_deref(), Some("Requirements"));
        goal.set_spec(None, now()).unwrap();
        assert_eq!(goal.spec, None);
        goal.cancel(now()).unwrap();
        assert!(goal.set_spec(Some("Late".into()), now()).is_err());
    }

    #[test]
    fn completion_requires_finished_tasks() {
        let mut goal = goal();
        let mut task = Task::new(
            "T1".into(),
            "F1".into(),
            "Work".into(),
            String::new(),
            String::new(),
            now(),
        )
        .unwrap();
        assert!(
            goal.complete(std::slice::from_ref(&task), now()).is_err(),
            "draft"
        );
        goal.activate(now()).unwrap();
        assert!(
            goal.complete(std::slice::from_ref(&task), now()).is_err(),
            "open task"
        );
        task.start(true, now()).unwrap();
        task.begin_testing(now()).unwrap();
        assert!(
            goal.complete(std::slice::from_ref(&task), now()).is_err(),
            "testing is still open"
        );
        task.pass_testing(now()).unwrap();
        task.finish(now()).unwrap();
        assert_eq!(task.status, TaskStatus::Done);
        goal.complete(std::slice::from_ref(&task), now()).unwrap();
        assert_eq!(goal.status, GoalStatus::Complete);
        assert!(goal.completed_at.is_some());
        assert!(goal.cancel(now()).is_err());
        assert!(goal.require_open().is_err());
    }
}
