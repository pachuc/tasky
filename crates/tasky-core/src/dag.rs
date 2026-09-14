use crate::{Dependency, Error, Result, Task, TaskStatus};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// A validated, in-memory view of every task and dependency in a project.
///
/// Storage loads rows, builds a `Dag`, and asks it questions. All traversal is iterative and
/// ordered by task ID so results are deterministic.
#[derive(Debug)]
pub struct Dag<'a> {
    tasks: BTreeMap<&'a str, &'a Task>,
    /// task -> tasks it depends on
    requires: BTreeMap<&'a str, BTreeSet<&'a str>>,
}

impl<'a> Dag<'a> {
    /// Build a graph, checking that every edge joins two existing tasks and nothing is cyclic.
    ///
    /// # Errors
    /// Returns an error for duplicate task IDs, dangling or self edges, or a cycle.
    pub fn new(tasks: &'a [Task], edges: &'a [Dependency]) -> Result<Self> {
        let mut index = BTreeMap::new();
        for task in tasks {
            if index.insert(task.id.as_str(), task).is_some() {
                return Err(Error::Duplicate(format!("task {}", task.id)));
            }
        }
        let mut requires: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for edge in edges {
            for id in [&edge.task_id, &edge.depends_on_id] {
                if !index.contains_key(id.as_str()) {
                    return Err(Error::NotFound(format!("task {id}")));
                }
            }
            if edge.task_id == edge.depends_on_id {
                return Err(Error::Invalid(format!(
                    "task {} depends on itself",
                    edge.task_id
                )));
            }
            requires
                .entry(edge.task_id.as_str())
                .or_default()
                .insert(edge.depends_on_id.as_str());
        }
        let dag = Self {
            tasks: index,
            requires,
        };
        if dag.topological_order().len() != dag.tasks.len() {
            return Err(Error::Invalid("dependency graph contains a cycle".into()));
        }
        Ok(dag)
    }

    /// Look up a task.
    ///
    /// # Errors
    /// Returns an error if the task does not exist.
    pub fn task(&self, id: &str) -> Result<&'a Task> {
        self.tasks
            .get(id)
            .copied()
            .ok_or_else(|| Error::NotFound(format!("task {id}")))
    }

    /// Every task in ID order.
    pub fn tasks(&self) -> impl Iterator<Item = &'a Task> + '_ {
        self.tasks.values().copied()
    }

    /// Direct prerequisites of a task, in ID order.
    pub fn dependencies<'b>(&'b self, id: &str) -> impl Iterator<Item = &'a Task> + 'b {
        self.requires
            .get(id)
            .into_iter()
            .flatten()
            .filter_map(move |dep| self.tasks.get(dep).copied())
    }

    /// Tasks that directly require the given task, in ID order.
    pub fn dependents<'b>(&'b self, id: &'b str) -> impl Iterator<Item = &'a Task> + 'b {
        self.requires
            .iter()
            .filter(move |(_, deps)| deps.contains(id))
            .filter_map(move |(task, _)| self.tasks.get(task).copied())
    }

    /// Check whether `task_id` may gain a dependency on `depends_on_id`.
    ///
    /// # Errors
    /// Returns an error if either task is missing, the edge is a self edge, the task has
    /// already started, or the edge would create a cycle.
    pub fn check_new_dependency(&self, task_id: &str, depends_on_id: &str) -> Result<()> {
        let task = self.task(task_id)?;
        self.task(depends_on_id)?;
        Dependency::new(task_id.into(), depends_on_id.into())?;
        task.require_editable_dependencies()?;
        if self.reaches(depends_on_id, task_id) {
            return Err(Error::Invalid(format!(
                "task {task_id} cannot depend on {depends_on_id}: it would create a cycle"
            )));
        }
        Ok(())
    }

    /// Whether every direct dependency of the task is done.
    ///
    /// # Errors
    /// Returns an error if the task does not exist.
    pub fn dependencies_done(&self, id: &str) -> Result<bool> {
        self.task(id)?;
        Ok(self
            .dependencies(id)
            .all(|dep| dep.status == TaskStatus::Done))
    }

    /// Direct dependencies that are not yet done, in ID order.
    pub fn blockers<'b>(&'b self, id: &str) -> impl Iterator<Item = &'a Task> + 'b {
        self.dependencies(id)
            .filter(|dep| dep.status != TaskStatus::Done)
    }

    /// Whether a task is todo with every dependency done.
    #[must_use]
    pub fn is_ready(&self, task: &Task) -> bool {
        task.status == TaskStatus::Todo && self.blockers(&task.id).next().is_none()
    }

    /// Ready tasks in ID order.
    pub fn ready(&self) -> impl Iterator<Item = &'a Task> + '_ {
        self.tasks().filter(|task| self.is_ready(task))
    }

    /// Kahn's algorithm with ID-ordered tie breaking. Prerequisites come before dependents.
    /// Returns fewer tasks than exist only when the graph is cyclic, which `new` rejects.
    #[must_use]
    pub fn topological_order(&self) -> Vec<&'a Task> {
        let mut remaining: BTreeMap<&str, usize> = self
            .tasks
            .keys()
            .map(|id| (*id, self.requires.get(id).map_or(0, BTreeSet::len)))
            .collect();
        let mut queue: VecDeque<&str> = remaining
            .iter()
            .filter(|(_, count)| **count == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut order = Vec::with_capacity(self.tasks.len());
        while let Some(id) = queue.pop_front() {
            order.push(self.tasks[id]);
            let mut released = BTreeSet::new();
            for (dependent, deps) in &self.requires {
                if deps.contains(id)
                    && let Some(count) = remaining.get_mut(dependent)
                {
                    *count -= 1;
                    if *count == 0 {
                        released.insert(*dependent);
                    }
                }
            }
            queue.extend(released);
        }
        order
    }

    /// Whether `from` transitively depends on `target`.
    fn reaches(&self, from: &str, target: &str) -> bool {
        let mut stack = vec![from];
        let mut visited = BTreeSet::new();
        while let Some(id) = stack.pop() {
            if id == target {
                return true;
            }
            if visited.insert(id)
                && let Some(deps) = self.requires.get(id)
            {
                stack.extend(deps.iter().copied());
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Timestamp;

    fn task(id: &str) -> Task {
        Task::new(
            id.into(),
            "F".into(),
            id.to_uppercase(),
            String::new(),
            String::new(),
            Timestamp::UNIX_EPOCH,
        )
        .unwrap()
    }

    /// Walk a todo task through the whole lifecycle to done.
    fn finish(task: &mut Task) {
        let now = Timestamp::UNIX_EPOCH;
        task.start(true, now).unwrap();
        task.begin_testing(now).unwrap();
        task.pass_testing(now).unwrap();
        task.finish(now).unwrap();
    }

    fn edge(task: &str, on: &str) -> Dependency {
        Dependency::new(task.into(), on.into()).unwrap()
    }

    fn ids<'a>(tasks: impl Iterator<Item = &'a Task>) -> Vec<&'a str> {
        tasks.map(|t| t.id.as_str()).collect()
    }

    #[test]
    fn readiness_follows_dependencies() {
        let mut tasks = vec![task("a"), task("b"), task("c")];
        let edges = vec![edge("b", "a"), edge("c", "b")];
        let dag = Dag::new(&tasks, &edges).unwrap();
        assert_eq!(ids(dag.ready()), vec!["a"]);
        assert_eq!(ids(dag.blockers("c")), vec!["b"]);
        assert_eq!(ids(dag.dependents("a")), vec!["b"]);
        assert_eq!(
            ids(dag.topological_order().into_iter()),
            vec!["a", "b", "c"]
        );
        drop(dag);

        finish(&mut tasks[0]);
        let dag = Dag::new(&tasks, &edges).unwrap();
        assert_eq!(ids(dag.ready()), vec!["b"]);
        assert!(dag.dependencies_done("b").unwrap());
        assert!(!dag.dependencies_done("c").unwrap());
    }

    #[test]
    fn cancelled_dependencies_keep_dependents_blocked() {
        let mut tasks = vec![task("a"), task("b")];
        tasks[0].cancel(Timestamp::UNIX_EPOCH).unwrap();
        let edges = vec![edge("b", "a")];
        let dag = Dag::new(&tasks, &edges).unwrap();
        assert!(dag.ready().next().is_none());
    }

    #[test]
    fn construction_rejects_bad_graphs() {
        let tasks = vec![task("a"), task("b")];
        assert!(Dag::new(&tasks, &[edge("a", "b"), edge("b", "a")]).is_err());
        assert!(Dag::new(&tasks, &[edge("a", "missing")]).is_err());
        assert!(Dag::new(&[task("a"), task("a")], &[]).is_err());
        let self_edge = Dependency {
            task_id: "a".into(),
            depends_on_id: "a".into(),
        };
        assert!(Dag::new(&tasks, &[self_edge]).is_err());
    }

    #[test]
    fn new_dependencies_are_checked_for_cycles_and_state() {
        let mut tasks = vec![task("a"), task("b"), task("c")];
        let edges = vec![edge("b", "a"), edge("c", "b")];
        let dag = Dag::new(&tasks, &edges).unwrap();
        assert!(dag.check_new_dependency("a", "c").is_err(), "cycle");
        assert!(dag.check_new_dependency("a", "a").is_err(), "self");
        assert!(dag.check_new_dependency("a", "zzz").is_err(), "missing");
        dag.check_new_dependency("c", "a").unwrap();
        drop(dag);

        tasks[0].start(true, Timestamp::UNIX_EPOCH).unwrap();
        let dag = Dag::new(&tasks, &edges).unwrap();
        assert!(dag.check_new_dependency("a", "c").is_err(), "started");
    }

    #[test]
    fn topological_order_breaks_ties_by_id() {
        let tasks = vec![task("d"), task("c"), task("b"), task("a")];
        let edges = vec![edge("a", "d"), edge("b", "d")];
        let dag = Dag::new(&tasks, &edges).unwrap();
        assert_eq!(
            ids(dag.topological_order().into_iter()),
            vec!["c", "d", "a", "b"]
        );
    }
}
