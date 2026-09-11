//! Deterministic task graph rules. No filesystem, UI, or process execution.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("task not found: {0}")]
    NotFound(String),
    #[error("task already exists: {0}")]
    Duplicate(String),
    #[error("invalid operation: {0}")]
    Invalid(String),
}

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    Pending,
    Running { agent: String },
    Done,
    Failed { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub dependencies: BTreeSet<String>,
    pub status: Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Graph {
    schema_version: u32,
    tasks: BTreeMap<String, Task>,
}

impl Default for Graph {
    fn default() -> Self {
        Self {
            schema_version: 1,
            tasks: BTreeMap::new(),
        }
    }
}

fn nonempty(value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::Invalid("value must not be blank".into()));
    }
    Ok(())
}

impl Graph {
    /// Iterate over tasks in ID order.
    pub fn tasks(&self) -> impl Iterator<Item = &Task> {
        self.tasks.values()
    }

    /// Look up a task by ID.
    ///
    /// # Errors
    /// Returns an error if the task does not exist.
    pub fn task(&self, id: &str) -> Result<&Task> {
        self.tasks.get(id).ok_or_else(|| Error::NotFound(id.into()))
    }

    /// Insert a pending task with no dependencies.
    ///
    /// # Errors
    /// Returns an error if the ID or title is blank, or the ID already exists.
    pub fn add(&mut self, id: String, title: String) -> Result<()> {
        nonempty(&id)?;
        nonempty(&title)?;
        if self.tasks.contains_key(&id) {
            return Err(Error::Duplicate(id));
        }
        self.tasks.insert(
            id.clone(),
            Task {
                id,
                title,
                dependencies: BTreeSet::new(),
                status: Status::Pending,
            },
        );
        Ok(())
    }

    /// Require `dependency` to finish before `id` can be claimed.
    ///
    /// # Errors
    /// Returns an error if either task is missing, `id` is not pending,
    /// or the dependency would create a cycle.
    pub fn depend(&mut self, id: &str, dependency: &str) -> Result<()> {
        self.require_pending(id)?;
        self.task(dependency)?;
        if self.reaches(dependency, id) {
            return Err(Error::Invalid("dependency would create a cycle".into()));
        }
        self.task_mut(id)?.dependencies.insert(dependency.into());
        Ok(())
    }

    /// Remove a prerequisite from a pending task.
    ///
    /// # Errors
    /// Returns an error if either task is missing or `id` is not pending.
    pub fn undepend(&mut self, id: &str, dependency: &str) -> Result<()> {
        self.require_pending(id)?;
        self.task(dependency)?;
        self.task_mut(id)?.dependencies.remove(dependency);
        Ok(())
    }

    fn task_mut(&mut self, id: &str) -> Result<&mut Task> {
        self.tasks
            .get_mut(id)
            .ok_or_else(|| Error::NotFound(id.into()))
    }

    fn reaches(&self, from: &str, target: &str) -> bool {
        let mut stack = vec![from];
        let mut visited = BTreeSet::new();
        while let Some(id) = stack.pop() {
            if id == target {
                return true;
            }
            if visited.insert(id)
                && let Some(task) = self.tasks.get(id)
            {
                stack.extend(task.dependencies.iter().map(String::as_str));
            }
        }
        false
    }

    fn require_pending(&self, id: &str) -> Result<()> {
        if self.task(id)?.status != Status::Pending {
            return Err(Error::Invalid("task must be pending".into()));
        }
        Ok(())
    }

    /// Whether a task is pending and all its prerequisites are done.
    #[must_use]
    pub fn is_ready(&self, task: &Task) -> bool {
        task.status == Status::Pending
            && task
                .dependencies
                .iter()
                .all(|id| self.tasks.get(id).is_some_and(|t| t.status == Status::Done))
    }

    /// Iterate over ready tasks in ID order.
    pub fn ready(&self) -> impl Iterator<Item = &Task> {
        self.tasks().filter(|task| self.is_ready(task))
    }

    /// Assign a ready task to an agent and mark it running.
    ///
    /// # Errors
    /// Returns an error if the agent is blank, the task is missing,
    /// or the task is not ready.
    pub fn claim(&mut self, id: &str, agent: String) -> Result<()> {
        nonempty(&agent)?;
        if !self.is_ready(self.task(id)?) {
            return Err(Error::Invalid("task is not ready".into()));
        }
        self.task_mut(id)?.status = Status::Running { agent };
        Ok(())
    }

    /// Complete a running task, or fail it with the supplied reason.
    ///
    /// # Errors
    /// Returns an error if the task is missing, is not running for this agent,
    /// or the failure reason is blank.
    pub fn finish(&mut self, id: &str, agent: &str, failure: Option<String>) -> Result<()> {
        match &self.task(id)?.status {
            Status::Running { agent: owner } if owner == agent => {}
            _ => {
                return Err(Error::Invalid(
                    "only the claiming agent can finish a running task".into(),
                ));
            }
        }
        let status = match failure {
            Some(reason) => {
                nonempty(&reason)?;
                Status::Failed { reason }
            }
            None => Status::Done,
        };
        self.task_mut(id)?.status = status;
        Ok(())
    }

    /// Return a failed task to pending.
    ///
    /// # Errors
    /// Returns an error if the task is missing or is not failed.
    pub fn retry(&mut self, id: &str) -> Result<()> {
        if !matches!(self.task(id)?.status, Status::Failed { .. }) {
            return Err(Error::Invalid("only failed tasks can be retried".into()));
        }
        self.task_mut(id)?.status = Status::Pending;
        Ok(())
    }

    /// Validate snapshots at the persistence boundary, including data written externally.
    ///
    /// # Errors
    /// Returns an error for an unsupported schema, blank required fields,
    /// mismatched task IDs, missing dependencies, cycles, or a started task
    /// with an unfinished prerequisite.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            return Err(Error::Invalid("unsupported schema version".into()));
        }
        for (id, task) in &self.tasks {
            nonempty(id)?;
            nonempty(&task.title)?;
            if id != &task.id {
                return Err(Error::Invalid("task key/id mismatch".into()));
            }
            for dependency in &task.dependencies {
                self.task(dependency)?;
                if self.reaches(dependency, id) {
                    return Err(Error::Invalid("graph contains a cycle".into()));
                }
            }
            match &task.status {
                Status::Running { agent } => nonempty(agent)?,
                Status::Failed { reason } => nonempty(reason)?,
                _ => {}
            }
            if task.status != Status::Pending
                && task
                    .dependencies
                    .iter()
                    .any(|d| self.tasks[d].status != Status::Done)
            {
                return Err(Error::Invalid(
                    "started task has unfinished dependencies".into(),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> Graph {
        let mut graph = Graph::default();
        graph.add("a".into(), "First".into()).unwrap();
        graph.add("b".into(), "Second".into()).unwrap();
        graph.depend("b", "a").unwrap();
        graph
    }

    #[test]
    fn dependencies_gate_claims_and_completion_unlocks_work() {
        let mut g = graph();
        assert!(g.claim("b", "agent".into()).is_err());
        g.claim("a", "agent".into()).unwrap();
        assert!(g.claim("a", "other".into()).is_err());
        assert!(g.finish("a", "other", None).is_err());
        g.finish("a", "agent", None).unwrap();
        assert_eq!(
            g.ready().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["b"]
        );
        g.validate().unwrap();
    }

    #[test]
    fn cycles_and_missing_dependencies_do_not_mutate_graph() {
        let mut g = graph();
        assert!(g.depend("a", "b").is_err());
        assert!(g.depend("a", "a").is_err());
        assert!(g.depend("a", "missing").is_err());
        assert!(g.task("a").unwrap().dependencies.is_empty());
        g.validate().unwrap();
    }

    #[test]
    fn failure_requires_explicit_retry() {
        let mut g = graph();
        g.claim("a", "agent".into()).unwrap();
        assert!(g.undepend("a", "b").is_err());
        g.finish("a", "agent", Some("network".into())).unwrap();
        assert_eq!(g.ready().count(), 0);
        g.retry("a").unwrap();
        assert_eq!(g.ready().count(), 1);
        assert!(g.retry("a").is_err());
    }
}
