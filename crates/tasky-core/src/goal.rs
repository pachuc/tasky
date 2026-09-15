use crate::{Error, GoalStatus, Result, Task, Timestamp, require_nonblank, validate_slug};
use serde::{Deserialize, Serialize};

/// A high-level goal within a project. Tasks attach directly to it, and it may hold
/// sub-goals without limit. A spec is optional.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    pub id: String,
    pub project_id: String,
    /// The goal this one sits inside, if any. Always in the same project.
    pub parent_id: Option<String>,
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
    /// Create a draft goal, optionally inside a parent goal of the same project.
    ///
    /// # Errors
    /// Returns an error if an ID is blank, the slug is malformed, the title is blank, or
    /// the parent is the goal itself.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        project_id: String,
        parent_id: Option<String>,
        slug: String,
        title: String,
        description: String,
        spec: Option<String>,
        now: Timestamp,
    ) -> Result<Self> {
        require_nonblank("id", &id)?;
        require_nonblank("project id", &project_id)?;
        if let Some(parent) = &parent_id {
            require_nonblank("parent id", parent)?;
            if *parent == id {
                return Err(Error::Invalid("a goal cannot be its own parent".into()));
            }
        }
        validate_slug(&slug)?;
        require_nonblank("title", &title)?;
        Ok(Self {
            id,
            project_id,
            parent_id,
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

    /// Whether the goal can still change: gain tasks, sub-goals, a new spec, or a new status.
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

    /// Complete an active goal whose own tasks are all finished and whose sub-goals are all
    /// closed. Because each sub-goal had to satisfy the same rule to close, this covers every
    /// task at any depth.
    ///
    /// # Errors
    /// Returns an error if the goal is not active, a task is still open, or a sub-goal is
    /// still open.
    pub fn complete(&mut self, tasks: &[Task], subgoals: &[Goal], now: Timestamp) -> Result<()> {
        if self.status != GoalStatus::Active {
            return Err(Error::Invalid(format!(
                "goal {} is {} and cannot be completed",
                self.slug, self.status
            )));
        }
        require_children_closed(tasks, subgoals)?;
        self.status = GoalStatus::Complete;
        self.updated_at = now;
        self.completed_at = Some(now);
        Ok(())
    }

    /// Reopen a complete goal so more tasks can be added to it. The completion time is
    /// cleared; the goal completes again once the new work is finished.
    ///
    /// # Errors
    /// Returns an error if the goal is not complete.
    pub fn reopen(&mut self, now: Timestamp) -> Result<()> {
        if self.status != GoalStatus::Complete {
            return Err(Error::Invalid(format!(
                "goal {} is {} and cannot be reopened",
                self.slug, self.status
            )));
        }
        self.status = GoalStatus::Active;
        self.updated_at = now;
        self.completed_at = None;
        Ok(())
    }

    /// Cancel a draft or active goal whose sub-goals are all closed. Its own tasks may stay
    /// open, as before; cancelling never invalidates finished work silently.
    ///
    /// # Errors
    /// Returns an error if the goal is already closed or a sub-goal is still open.
    pub fn cancel(&mut self, subgoals: &[Goal], now: Timestamp) -> Result<()> {
        self.require_open()?;
        require_children_closed(&[], subgoals)?;
        self.status = GoalStatus::Cancelled;
        self.updated_at = now;
        Ok(())
    }
}

/// Every task must be done or cancelled and every sub-goal complete or cancelled.
fn require_children_closed(tasks: &[Task], subgoals: &[Goal]) -> Result<()> {
    if let Some(task) = tasks.iter().find(|task| task.status.is_open()) {
        return Err(Error::Invalid(format!(
            "task {} ({}) is still {}",
            task.id, task.title, task.status
        )));
    }
    if let Some(goal) = subgoals.iter().find(|goal| !goal.status.is_closed()) {
        return Err(Error::Invalid(format!(
            "sub-goal {} ({}) is still {}",
            goal.slug, goal.title, goal.status
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TaskStatus;

    fn now() -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    fn goal_named(id: &str, parent: Option<&str>) -> Goal {
        Goal::new(
            id.into(),
            "P1".into(),
            parent.map(str::to_owned),
            id.to_lowercase(),
            id.to_uppercase(),
            String::new(),
            None,
            now(),
        )
        .unwrap()
    }

    fn goal() -> Goal {
        goal_named("F1", None)
    }

    fn done_task(id: &str) -> Task {
        let mut task = Task::new(
            id.into(),
            "F1".into(),
            "Work".into(),
            String::new(),
            String::new(),
            now(),
        )
        .unwrap();
        task.start(true, now()).unwrap();
        task.begin_testing(now()).unwrap();
        task.pass_testing(now()).unwrap();
        task.finish(now()).unwrap();
        task
    }

    #[test]
    fn creation_validates_fields() {
        let new = |slug: &str, title: &str, parent: Option<&str>| {
            Goal::new(
                "F".into(),
                "P".into(),
                parent.map(str::to_owned),
                slug.into(),
                title.into(),
                String::new(),
                None,
                now(),
            )
        };
        assert!(new("Bad Slug", "T", None).is_err());
        assert!(new("ok", " ", None).is_err());
        assert!(new("ok", "T", Some("F")).is_err(), "self parent");
        assert_eq!(
            new("ok", "T", Some("G")).unwrap().parent_id.as_deref(),
            Some("G")
        );
        let with_spec = Goal::new(
            "F".into(),
            "P".into(),
            None,
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
        goal.cancel(&[], now()).unwrap();
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
        let one = |task: &Task| vec![task.clone()];
        assert!(goal.complete(&one(&task), &[], now()).is_err(), "draft");
        goal.activate(now()).unwrap();
        assert!(goal.complete(&one(&task), &[], now()).is_err(), "open task");
        task.start(true, now()).unwrap();
        task.begin_testing(now()).unwrap();
        assert!(
            goal.complete(&one(&task), &[], now()).is_err(),
            "testing is still open"
        );
        task.pass_testing(now()).unwrap();
        task.finish(now()).unwrap();
        assert_eq!(task.status, TaskStatus::Done);
        goal.complete(&one(&task), &[], now()).unwrap();
        assert_eq!(goal.status, GoalStatus::Complete);
        assert!(goal.completed_at.is_some());
        assert!(goal.cancel(&[], now()).is_err());
        assert!(goal.require_open().is_err());
    }

    #[test]
    fn closing_requires_closed_subgoals() {
        let mut parent = goal_named("G", None);
        parent.activate(now()).unwrap();
        let mut child = goal_named("C", Some("G"));
        let tasks = [done_task("T")];
        assert!(
            parent
                .complete(&tasks, std::slice::from_ref(&child), now())
                .is_err(),
            "draft sub-goal keeps the parent open"
        );
        assert!(
            parent.cancel(std::slice::from_ref(&child), now()).is_err(),
            "cancel also waits for sub-goals"
        );
        child.cancel(&[], now()).unwrap();
        parent
            .complete(&tasks, std::slice::from_ref(&child), now())
            .unwrap();
        assert_eq!(parent.status, GoalStatus::Complete);

        let mut other = goal_named("H", None);
        let mut done_child = goal_named("D", Some("H"));
        done_child.activate(now()).unwrap();
        done_child.complete(&[], &[], now()).unwrap();
        other
            .cancel(std::slice::from_ref(&done_child), now())
            .unwrap();
        assert_eq!(other.status, GoalStatus::Cancelled);
    }

    #[test]
    fn a_complete_goal_can_be_reopened_and_completed_again() {
        let mut goal = goal_named("G1", None);
        assert!(goal.reopen(now()).is_err());
        goal.activate(now()).unwrap();
        assert!(goal.reopen(now()).is_err());
        goal.complete(&[], &[], now()).unwrap();
        goal.reopen(now()).unwrap();
        assert_eq!(goal.status, GoalStatus::Active);
        assert!(goal.completed_at.is_none());
        goal.complete(&[], &[], now()).unwrap();
        assert_eq!(goal.status, GoalStatus::Complete);
    }
}
