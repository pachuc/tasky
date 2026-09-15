//! SQLite persistence through Diesel.
//!
//! Every rule lives in `tasky-core`. This crate loads rows, applies a domain operation, and
//! writes the result inside one immediate transaction, so concurrent processes serialize on
//! SQLite's own lock instead of a hand-rolled one. Foreign keys are enabled per connection.
//!
//! One database holds any number of projects. It lives outside any repository, under the
//! user's data directory by default; see [`default_path`].

mod models;
mod schema;

use anyhow::{Context, Result, anyhow, ensure};
use diesel::prelude::*;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use models::{DependencyRow, GoalRow, LinkRow, ProjectRow, TaskRow};
use schema::{goals, projects, task_dependencies, task_links, tasks};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};
use tasky_core::{
    Dag, Dependency, Error, Goal, LinkKind, Project, Task, TaskLink, TaskStatus, Timestamp,
};

const MIGRATIONS: EmbeddedMigrations = embed_migrations!();

/// Where the database lives unless the caller says otherwise: `tasky/tasky.db` under
/// `$XDG_DATA_HOME`, falling back to `~/.local/share`, and finally the current directory.
#[must_use]
pub fn default_path() -> PathBuf {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::home_dir().map(|home| home.join(".local").join("share")))
        .unwrap_or_default();
    data_home.join("tasky").join("tasky.db")
}

/// How one optional text field should change in an update.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Change {
    /// Leave the field as it is.
    #[default]
    Keep,
    /// Remove the value.
    Clear,
    /// Store this value; blank text clears instead.
    Set(String),
}

impl Change {
    fn apply(self, set: impl FnOnce(Option<String>)) {
        match self {
            Self::Keep => {}
            Self::Clear => set(None),
            Self::Set(value) => set(Some(value)),
        }
    }
}

/// One open connection to a Tasky database.
pub struct Store {
    conn: SqliteConnection,
}

/// Which tasks a graph query should return. Both fields are references, resolved like on the
/// command line; a goal given alongside a project must belong to it.
#[derive(Debug, Default, Clone)]
pub struct TaskScope {
    pub project: Option<String>,
    pub goal: Option<String>,
}

/// Optional narrowing for task listings.
#[derive(Debug, Default, Clone)]
pub struct TaskFilter {
    pub scope: TaskScope,
    pub status: Option<TaskStatus>,
}

/// A task with everything a reader needs to act on it.
#[derive(Debug, Clone, Serialize)]
pub struct TaskDetail {
    #[serde(flatten)]
    pub task: Task,
    /// Project path, such as `app/mobile`.
    pub project: String,
    /// Goal slug.
    pub goal: String,
    pub ready: bool,
    pub depends_on: Vec<String>,
    pub blocked_by: Vec<String>,
    pub dependents: Vec<String>,
    pub links: Vec<TaskLink>,
}

/// Task totals by status.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct TaskCounts {
    pub todo: usize,
    pub in_progress: usize,
    pub testing: usize,
    pub ready_for_merge: usize,
    pub done: usize,
    pub cancelled: usize,
}

impl TaskCounts {
    fn of(tasks: &[Task]) -> Self {
        let mut counts = Self::default();
        for task in tasks {
            match task.status {
                TaskStatus::Todo => counts.todo += 1,
                TaskStatus::InProgress => counts.in_progress += 1,
                TaskStatus::Testing => counts.testing += 1,
                TaskStatus::ReadyForMerge => counts.ready_for_merge += 1,
                TaskStatus::Done => counts.done += 1,
                TaskStatus::Cancelled => counts.cancelled += 1,
            }
        }
        counts
    }
}

/// A goal with its project path, direct sub-goal count, and task totals over its whole
/// subtree.
#[derive(Debug, Clone, Serialize)]
pub struct GoalDetail {
    #[serde(flatten)]
    pub goal: Goal,
    /// Project path, such as `app/mobile`.
    pub project: String,
    pub subgoals: usize,
    pub tasks: TaskCounts,
}

/// A project with its path, direct sub-project and goal counts, and task totals over its
/// whole subtree.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectDetail {
    #[serde(flatten)]
    pub project: Project,
    /// Path from the root project, such as `app/mobile`.
    pub path: String,
    pub subprojects: usize,
    pub goals: usize,
    pub tasks: TaskCounts,
}

fn now() -> Timestamp {
    Timestamp::now()
}

fn new_id() -> String {
    ulid::Ulid::generate().to_string()
}

/// Normalize a user-typed ID fragment. ULIDs are Crockford base32, so anything else is a typo.
///
/// Returns `LIKE` patterns for a prefix and a suffix match. A ULID starts with a millisecond
/// timestamp, so IDs created close together share a long prefix; the random tail is what a
/// person can type to tell them apart.
fn id_patterns(reference: &str) -> Result<(String, String)> {
    ensure!(
        !reference.is_empty() && reference.bytes().all(|b| b.is_ascii_alphanumeric()),
        Error::Invalid(format!("{reference:?} is not an ID fragment"))
    );
    let fragment = reference.to_ascii_uppercase();
    Ok((format!("{fragment}%"), format!("%{fragment}")))
}

fn single<T>(mut rows: Vec<T>, kind: &str, reference: &str) -> Result<T> {
    match rows.len() {
        0 => Err(Error::NotFound(format!("{kind} {reference}")).into()),
        1 => Ok(rows.remove(0)),
        _ => Err(Error::Ambiguous(format!("{kind} {reference}")).into()),
    }
}

fn project_by_id(conn: &mut SqliteConnection, id: &str) -> Result<Project> {
    projects::table
        .find(id)
        .select(ProjectRow::as_select())
        .first(conn)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("project {id}")))?
        .try_into()
}

/// The child of `parent` (or a root when `None`) with this slug, if any.
fn child_project(
    conn: &mut SqliteConnection,
    parent: Option<&str>,
    slug: &str,
) -> Result<Option<Project>> {
    let mut query = projects::table.filter(projects::slug.eq(slug)).into_boxed();
    query = match parent {
        Some(parent) => query.filter(projects::parent_id.eq(parent.to_owned())),
        None => query.filter(projects::parent_id.is_null()),
    };
    query
        .select(ProjectRow::as_select())
        .first(conn)
        .optional()?
        .map(Project::try_from)
        .transpose()
}

/// Resolve a project by path (`app` or `app/mobile/ios`), or by a unique prefix or suffix
/// of its ID when the reference has no slash.
fn find_project(conn: &mut SqliteConnection, reference: &str) -> Result<Project> {
    let mut segments = reference.split('/');
    let first = segments.next().unwrap_or_default();
    let mut current = child_project(conn, None, first)?;
    if current.is_none() && !reference.contains('/') {
        let (prefix, suffix) = id_patterns(reference)?;
        let rows = projects::table
            .filter(projects::id.like(prefix).or(projects::id.like(suffix)))
            .order(projects::id)
            .limit(2)
            .select(ProjectRow::as_select())
            .load(conn)?;
        return single(rows, "project", reference)?.try_into();
    }
    for segment in segments {
        let Some(parent) = current else {
            break;
        };
        current = child_project(conn, Some(&parent.id), segment)?;
    }
    current.ok_or_else(|| Error::NotFound(format!("project {reference}")).into())
}

/// Slugs from the root down to this project, joined with `/`.
fn project_path(conn: &mut SqliteConnection, project: &Project) -> Result<String> {
    let mut segments = vec![project.slug.clone()];
    let mut parent = project.parent_id.clone();
    let mut hops = 0;
    while let Some(id) = parent {
        let ancestor = project_by_id(conn, &id)?;
        segments.push(ancestor.slug);
        parent = ancestor.parent_id;
        hops += 1;
        ensure!(hops < 1000, "project parent chain is cyclic");
    }
    segments.reverse();
    Ok(segments.join("/"))
}

/// The top-most ancestor of a project. Dependencies may join tasks anywhere under one root.
fn root_project_id(conn: &mut SqliteConnection, project_id: &str) -> Result<String> {
    let mut project = project_by_id(conn, project_id)?;
    let mut hops = 0;
    while let Some(parent) = project.parent_id.clone() {
        project = project_by_id(conn, &parent)?;
        hops += 1;
        ensure!(hops < 1000, "project parent chain is cyclic");
    }
    Ok(project.id)
}

/// Every project, in creation order.
fn load_projects(conn: &mut SqliteConnection) -> Result<Vec<Project>> {
    projects::table
        .order(projects::id)
        .select(ProjectRow::as_select())
        .load(conn)?
        .into_iter()
        .map(Project::try_from)
        .collect()
}

/// IDs of `root` and every project beneath it, breadth first.
fn project_subtree(conn: &mut SqliteConnection, root: &str) -> Result<Vec<String>> {
    let projects = load_projects(conn)?;
    Ok(subtree(
        root,
        projects
            .iter()
            .map(|p| (p.id.as_str(), p.parent_id.as_deref())),
    ))
}

/// IDs of `root` and every goal beneath it, breadth first.
fn goal_subtree(conn: &mut SqliteConnection, root: &str) -> Result<Vec<String>> {
    let goals = load_goals(conn, None)?;
    Ok(subtree(
        root,
        goals
            .iter()
            .map(|g| (g.id.as_str(), g.parent_id.as_deref())),
    ))
}

/// Breadth-first walk over (id, parent) pairs starting at `root`.
fn subtree<'a>(root: &str, edges: impl Iterator<Item = (&'a str, Option<&'a str>)>) -> Vec<String> {
    let edges: Vec<(&str, Option<&str>)> = edges.collect();
    let mut found = vec![root.to_owned()];
    let mut index = 0;
    while index < found.len() {
        let parent = found[index].clone();
        for (id, parent_id) in &edges {
            if *parent_id == Some(parent.as_str()) && !found.iter().any(|f| f == id) {
                found.push((*id).to_owned());
            }
        }
        index += 1;
    }
    found
}

/// Resolve a goal reference: `PROJECT/slug` where the project part is a project path, a slug
/// that is unique across every project, or a unique prefix or suffix of its ID.
fn find_goal(conn: &mut SqliteConnection, reference: &str) -> Result<Goal> {
    if let Some((project, slug)) = reference.rsplit_once('/') {
        let project = find_project(conn, project)?;
        return goals::table
            .filter(goals::project_id.eq(&project.id))
            .filter(goals::slug.eq(slug))
            .select(GoalRow::as_select())
            .first(conn)
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("goal {reference}")))?
            .try_into();
    }
    let by_slug = goals::table
        .filter(goals::slug.eq(reference))
        .order(goals::id)
        .limit(2)
        .select(GoalRow::as_select())
        .load(conn)?;
    match by_slug.len() {
        0 => {}
        1 => return single(by_slug, "goal", reference)?.try_into(),
        _ => {
            return Err(Error::Ambiguous(format!(
                "goal {reference} exists in more than one project; use PROJECT/{reference}"
            ))
            .into());
        }
    }
    let (prefix, suffix) = id_patterns(reference)?;
    let rows = goals::table
        .filter(goals::id.like(prefix).or(goals::id.like(suffix)))
        .order(goals::id)
        .limit(2)
        .select(GoalRow::as_select())
        .load(conn)?;
    single(rows, "goal", reference)?.try_into()
}

fn goal_by_id(conn: &mut SqliteConnection, id: &str) -> Result<Goal> {
    goals::table
        .find(id)
        .select(GoalRow::as_select())
        .first(conn)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("goal {id}")))?
        .try_into()
}

fn find_task(conn: &mut SqliteConnection, reference: &str) -> Result<Task> {
    let (prefix, suffix) = id_patterns(reference)?;
    let rows = tasks::table
        .filter(tasks::id.like(prefix).or(tasks::id.like(suffix)))
        .order(tasks::id)
        .limit(2)
        .select(TaskRow::as_select())
        .load(conn)?;
    single(rows, "task", reference)?.try_into()
}

fn load_goals(conn: &mut SqliteConnection, project_id: Option<&str>) -> Result<Vec<Goal>> {
    let mut query = goals::table.order(goals::id).into_boxed();
    if let Some(project_id) = project_id {
        query = query.filter(goals::project_id.eq(project_id.to_owned()));
    }
    query
        .select(GoalRow::as_select())
        .load(conn)?
        .into_iter()
        .map(Goal::try_from)
        .collect()
}

/// Goals of every project in `project_ids`, in creation order.
fn goals_in_projects(conn: &mut SqliteConnection, project_ids: &[String]) -> Result<Vec<Goal>> {
    goals::table
        .filter(goals::project_id.eq_any(project_ids))
        .order(goals::id)
        .select(GoalRow::as_select())
        .load(conn)?
        .into_iter()
        .map(Goal::try_from)
        .collect()
}

/// Direct sub-goals of a goal, in creation order.
fn load_subgoals(conn: &mut SqliteConnection, goal_id: &str) -> Result<Vec<Goal>> {
    goals::table
        .filter(goals::parent_id.eq(goal_id))
        .order(goals::id)
        .select(GoalRow::as_select())
        .load(conn)?
        .into_iter()
        .map(Goal::try_from)
        .collect()
}

/// Tasks of every goal in `goal_ids`, in creation order.
fn tasks_in_goals(conn: &mut SqliteConnection, goal_ids: &[String]) -> Result<Vec<Task>> {
    tasks::table
        .filter(tasks::goal_id.eq_any(goal_ids))
        .order(tasks::id)
        .select(TaskRow::as_select())
        .load(conn)?
        .into_iter()
        .map(Task::try_from)
        .collect()
}

fn load_tasks(conn: &mut SqliteConnection, goal_id: Option<&str>) -> Result<Vec<Task>> {
    let mut query = tasks::table.order(tasks::id).into_boxed();
    if let Some(goal_id) = goal_id {
        query = query.filter(tasks::goal_id.eq(goal_id.to_owned()));
    }
    query
        .select(TaskRow::as_select())
        .load(conn)?
        .into_iter()
        .map(Task::try_from)
        .collect()
}

fn load_dependencies(conn: &mut SqliteConnection) -> Result<Vec<Dependency>> {
    task_dependencies::table
        .order((task_dependencies::task_id, task_dependencies::depends_on_id))
        .select(DependencyRow::as_select())
        .load(conn)?
        .into_iter()
        .map(Dependency::try_from)
        .collect()
}

/// Every task and edge in the database. Edges never cross root projects, so the union of
/// all projects is still a DAG and callers build one [`Dag`] from the pair.
fn load_graph(conn: &mut SqliteConnection) -> Result<(Vec<Task>, Vec<Dependency>)> {
    Ok((load_tasks(conn, None)?, load_dependencies(conn)?))
}

fn load_links(conn: &mut SqliteConnection, task_id: &str) -> Result<Vec<TaskLink>> {
    task_links::table
        .filter(task_links::task_id.eq(task_id))
        .order(task_links::id)
        .select(LinkRow::as_select())
        .load(conn)?
        .into_iter()
        .map(TaskLink::try_from)
        .collect()
}

fn save_goal(conn: &mut SqliteConnection, goal: &Goal) -> Result<()> {
    diesel::update(goals::table.find(&goal.id))
        .set(GoalRow::from(goal))
        .execute(conn)?;
    Ok(())
}

fn save_task(conn: &mut SqliteConnection, task: &Task) -> Result<()> {
    diesel::update(tasks::table.find(&task.id))
        .set(TaskRow::from(task))
        .execute(conn)?;
    Ok(())
}

/// Resolve a scope to the goal IDs whose tasks it keeps; `None` keeps everything. A project
/// covers every project beneath it and a goal covers every sub-goal beneath it.
fn scope_goals(conn: &mut SqliteConnection, scope: &TaskScope) -> Result<Option<BTreeSet<String>>> {
    let project = scope
        .project
        .as_deref()
        .map(|reference| find_project(conn, reference))
        .transpose()?;
    if let Some(reference) = scope.goal.as_deref() {
        let goal = find_goal(conn, reference)?;
        if let Some(project) = &project {
            let within = project_subtree(conn, &project.id)?;
            ensure!(
                within.contains(&goal.project_id),
                Error::NotFound(format!("goal {reference} in project {}", project.slug))
            );
        }
        return Ok(Some(goal_subtree(conn, &goal.id)?.into_iter().collect()));
    }
    let Some(project) = project else {
        return Ok(None);
    };
    let projects = project_subtree(conn, &project.id)?;
    Ok(Some(
        goals_in_projects(conn, &projects)?
            .into_iter()
            .map(|goal| goal.id)
            .collect(),
    ))
}

impl Store {
    /// Create a new, empty database. Refuses to touch an existing file.
    ///
    /// # Errors
    /// Returns an error if the file exists or SQLite cannot be opened.
    pub fn init(path: &Path) -> Result<Self> {
        ensure!(
            !path.exists(),
            "database already exists at {}",
            path.display()
        );
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut store = Self::connect(path)?;
        store.apply_migrations()?;
        Ok(store)
    }

    /// Open an existing, fully migrated database. Never changes the schema.
    ///
    /// # Errors
    /// Returns an error if the file is missing, cannot be opened, or has pending migrations.
    pub fn open(path: &Path) -> Result<Self> {
        let mut store = Self::connect_existing(path)?;
        let pending = store
            .conn
            .has_pending_migration(MIGRATIONS)
            .map_err(|error| anyhow!("checking migrations: {error}"))?;
        ensure!(
            !pending,
            "database at {} needs a schema upgrade; run `tasky migrate`",
            path.display()
        );
        Ok(store)
    }

    /// Apply pending migrations to an existing database and return their versions.
    ///
    /// # Errors
    /// Returns an error if the file is missing or a migration fails.
    pub fn migrate(path: &Path) -> Result<Vec<String>> {
        Self::connect_existing(path)?.apply_migrations()
    }

    fn connect_existing(path: &Path) -> Result<Self> {
        ensure!(
            path.is_file(),
            "no tasky database at {}; run `tasky init` first",
            path.display()
        );
        Self::connect(path)
    }

    fn connect(path: &Path) -> Result<Self> {
        let url = path.to_str().context("database path is not valid UTF-8")?;
        let mut conn = SqliteConnection::establish(url)
            .with_context(|| format!("opening {}", path.display()))?;
        diesel::sql_query("PRAGMA foreign_keys = ON").execute(&mut conn)?;
        diesel::sql_query("PRAGMA busy_timeout = 5000").execute(&mut conn)?;
        Ok(Self { conn })
    }

    fn apply_migrations(&mut self) -> Result<Vec<String>> {
        let applied = self
            .conn
            .run_pending_migrations(MIGRATIONS)
            .map_err(|error| anyhow!("applying migrations: {error}"))?;
        Ok(applied.iter().map(ToString::to_string).collect())
    }

    /// Create a project, optionally inside a parent project and optionally tied to a
    /// repository by local path and/or remote URL.
    ///
    /// # Errors
    /// Returns an error if the parent cannot be resolved, the slug is malformed or already
    /// used by a sibling, or the name is blank.
    pub fn create_project(
        &mut self,
        parent: Option<&str>,
        slug: String,
        name: String,
        repo_path: Option<String>,
        repo_url: Option<String>,
    ) -> Result<Project> {
        self.conn.immediate_transaction(|conn| {
            let parent = parent
                .map(|reference| find_project(conn, reference))
                .transpose()?;
            let parent_id = parent.as_ref().map(|parent| parent.id.clone());
            let project =
                Project::new(new_id(), parent_id, slug, name, repo_path, repo_url, now())?;
            if child_project(conn, project.parent_id.as_deref(), &project.slug)?.is_some() {
                let path = match &parent {
                    Some(parent) => format!("{}/{}", project_path(conn, parent)?, project.slug),
                    None => project.slug.clone(),
                };
                return Err(Error::Duplicate(format!("project {path}")).into());
            }
            diesel::insert_into(projects::table)
                .values(ProjectRow::from(&project))
                .execute(conn)?;
            Ok(project)
        })
    }

    /// Every project in creation order, roots and nested alike.
    ///
    /// # Errors
    /// Returns an error if a row cannot be read.
    pub fn projects(&mut self) -> Result<Vec<Project>> {
        load_projects(&mut self.conn)
    }

    /// Look up a project by path such as `app/mobile`, or by a unique prefix or suffix of
    /// its ID.
    ///
    /// # Errors
    /// Returns an error if nothing or more than one project matches.
    pub fn project(&mut self, reference: &str) -> Result<Project> {
        find_project(&mut self.conn, reference)
    }

    /// Change the repository path and/or URL a project is tied to, in one transaction.
    ///
    /// # Errors
    /// Returns an error if the project cannot be resolved.
    pub fn set_project_repo(
        &mut self,
        reference: &str,
        path: Change,
        url: Change,
    ) -> Result<Project> {
        self.conn.immediate_transaction(|conn| {
            let mut project = find_project(conn, reference)?;
            path.apply(|value| project.set_repo_path(value));
            url.apply(|value| project.set_repo_url(value));
            diesel::update(projects::table.find(&project.id))
                .set(ProjectRow::from(&project))
                .execute(conn)?;
            Ok(project)
        })
    }

    /// A project with its path, direct sub-project and goal counts, and task totals over
    /// everything beneath it.
    ///
    /// # Errors
    /// Returns an error if the project cannot be resolved.
    pub fn project_detail(&mut self, reference: &str) -> Result<ProjectDetail> {
        let conn = &mut self.conn;
        let project = find_project(conn, reference)?;
        let path = project_path(conn, &project)?;
        let subtree = project_subtree(conn, &project.id)?;
        let subprojects = load_projects(conn)?
            .iter()
            .filter(|p| p.parent_id.as_deref() == Some(project.id.as_str()))
            .count();
        let goals = load_goals(conn, Some(&project.id))?.len();
        let goal_ids: Vec<String> = goals_in_projects(conn, &subtree)?
            .into_iter()
            .map(|goal| goal.id)
            .collect();
        let tasks = tasks_in_goals(conn, &goal_ids)?;
        Ok(ProjectDetail {
            project,
            path,
            subprojects,
            goals,
            tasks: TaskCounts::of(&tasks),
        })
    }

    /// Create a draft goal in a project, optionally inside a parent goal of that project and
    /// optionally with a spec.
    ///
    /// # Errors
    /// Returns an error if the project or parent cannot be resolved, the parent is in another
    /// project or already closed, the slug is malformed or taken within the project, or the
    /// title is blank.
    pub fn create_goal(
        &mut self,
        project: &str,
        parent: Option<&str>,
        slug: String,
        title: String,
        description: String,
        spec: Option<String>,
    ) -> Result<Goal> {
        self.conn.immediate_transaction(|conn| {
            let project = find_project(conn, project)?;
            let parent = parent
                .map(|reference| find_goal(conn, reference))
                .transpose()?;
            if let Some(parent) = &parent {
                ensure!(
                    parent.project_id == project.id,
                    Error::Invalid(format!(
                        "parent goal {} belongs to a different project",
                        parent.slug
                    ))
                );
                parent.require_open()?;
            }
            let goal = Goal::new(
                new_id(),
                project.id.clone(),
                parent.map(|parent| parent.id),
                slug,
                title,
                description,
                spec,
                now(),
            )?;
            let taken: i64 = goals::table
                .filter(goals::project_id.eq(&project.id))
                .filter(goals::slug.eq(&goal.slug))
                .count()
                .get_result(conn)?;
            ensure!(
                taken == 0,
                Error::Duplicate(format!("goal {}/{}", project.slug, goal.slug))
            );
            diesel::insert_into(goals::table)
                .values(GoalRow::from(&goal))
                .execute(conn)?;
            Ok(goal)
        })
    }

    /// Goals in creation order, optionally within one project and the projects beneath it.
    ///
    /// # Errors
    /// Returns an error if the project cannot be resolved or a row cannot be read.
    pub fn goals(&mut self, project: Option<&str>) -> Result<Vec<Goal>> {
        let conn = &mut self.conn;
        let Some(reference) = project else {
            return load_goals(conn, None);
        };
        let project = find_project(conn, reference)?;
        let subtree = project_subtree(conn, &project.id)?;
        goals_in_projects(conn, &subtree)
    }

    /// Look up a goal by `PROJECT/slug`, by a slug unique across projects, or by a unique
    /// prefix or suffix of its ID.
    ///
    /// # Errors
    /// Returns an error if nothing or more than one goal matches.
    pub fn goal(&mut self, reference: &str) -> Result<Goal> {
        find_goal(&mut self.conn, reference)
    }

    /// A goal with its project path, direct sub-goal count, and task totals over everything
    /// beneath it.
    ///
    /// # Errors
    /// Returns an error if the goal cannot be resolved.
    pub fn goal_detail(&mut self, reference: &str) -> Result<GoalDetail> {
        let conn = &mut self.conn;
        let goal = find_goal(conn, reference)?;
        let project = project_by_id(conn, &goal.project_id)?;
        let project = project_path(conn, &project)?;
        let subgoals = load_subgoals(conn, &goal.id)?.len();
        let subtree = goal_subtree(conn, &goal.id)?;
        let tasks = tasks_in_goals(conn, &subtree)?;
        Ok(GoalDetail {
            goal,
            project,
            subgoals,
            tasks: TaskCounts::of(&tasks),
        })
    }

    fn update_goal(
        &mut self,
        reference: &str,
        apply: impl FnOnce(&mut Goal, &mut SqliteConnection) -> Result<()>,
    ) -> Result<Goal> {
        self.conn.immediate_transaction(|conn| {
            let mut goal = find_goal(conn, reference)?;
            apply(&mut goal, conn)?;
            save_goal(conn, &goal)?;
            Ok(goal)
        })
    }

    /// Replace or clear the spec of an open goal.
    ///
    /// # Errors
    /// Returns an error if the goal cannot be resolved or is complete or cancelled.
    pub fn set_goal_spec(&mut self, reference: &str, spec: Option<String>) -> Result<Goal> {
        self.update_goal(reference, |goal, _| Ok(goal.set_spec(spec, now())?))
    }

    /// Activate a draft goal.
    ///
    /// # Errors
    /// Returns an error if the goal cannot be resolved or the transition is invalid.
    pub fn activate_goal(&mut self, reference: &str) -> Result<Goal> {
        self.update_goal(reference, |goal, _| Ok(goal.activate(now())?))
    }

    /// Complete an active goal whose own tasks are all finished and whose sub-goals are all
    /// closed.
    ///
    /// # Errors
    /// Returns an error if the goal cannot be resolved or the transition is invalid.
    pub fn complete_goal(&mut self, reference: &str) -> Result<Goal> {
        self.update_goal(reference, |goal, conn| {
            let tasks = load_tasks(conn, Some(&goal.id))?;
            let subgoals = load_subgoals(conn, &goal.id)?;
            Ok(goal.complete(&tasks, &subgoals, now())?)
        })
    }

    /// Reopen a complete goal so new tasks can be added. A sub-goal can only be reopened while
    /// its parent goal is still open, so a complete parent never hides active work.
    ///
    /// # Errors
    /// Returns an error if the goal cannot be resolved, is not complete, or its parent is
    /// closed.
    pub fn reopen_goal(&mut self, reference: &str) -> Result<Goal> {
        self.update_goal(reference, |goal, conn| {
            if let Some(parent_id) = &goal.parent_id {
                let parent = find_goal(conn, parent_id)?;
                parent.require_open()?;
            }
            Ok(goal.reopen(now())?)
        })
    }

    /// Cancel a draft or active goal whose sub-goals are all closed.
    ///
    /// # Errors
    /// Returns an error if the goal cannot be resolved or the transition is invalid.
    pub fn cancel_goal(&mut self, reference: &str) -> Result<Goal> {
        self.update_goal(reference, |goal, conn| {
            let subgoals = load_subgoals(conn, &goal.id)?;
            Ok(goal.cancel(&subgoals, now())?)
        })
    }

    /// Add a todo task to a goal.
    ///
    /// # Errors
    /// Returns an error if the goal cannot be resolved, is closed, or the title is blank.
    pub fn create_task(
        &mut self,
        goal: &str,
        title: String,
        body: String,
        test_plan: String,
    ) -> Result<Task> {
        self.conn.immediate_transaction(|conn| {
            let goal = find_goal(conn, goal)?;
            goal.require_open()?;
            let task = Task::new(new_id(), goal.id, title, body, test_plan, now())?;
            diesel::insert_into(tasks::table)
                .values(TaskRow::from(&task))
                .execute(conn)?;
            Ok(task)
        })
    }

    /// Tasks in ID order, optionally narrowed by project, goal, and status.
    ///
    /// # Errors
    /// Returns an error if a reference cannot be resolved or a row cannot be read.
    pub fn tasks(&mut self, filter: &TaskFilter) -> Result<Vec<Task>> {
        let conn = &mut self.conn;
        let keep = scope_goals(conn, &filter.scope)?;
        Ok(load_tasks(conn, None)?
            .into_iter()
            .filter(|task| keep.as_ref().is_none_or(|ids| ids.contains(&task.goal_id)))
            .filter(|task| filter.status.is_none_or(|status| task.status == status))
            .collect())
    }

    /// Look up a task by a unique prefix or suffix of its ID.
    ///
    /// # Errors
    /// Returns an error if nothing or more than one task matches.
    pub fn task(&mut self, reference: &str) -> Result<Task> {
        find_task(&mut self.conn, reference)
    }

    /// A task with its project, goal, readiness, neighbours, and links.
    ///
    /// # Errors
    /// Returns an error if the task cannot be resolved.
    pub fn task_detail(&mut self, reference: &str) -> Result<TaskDetail> {
        let conn = &mut self.conn;
        let task = find_task(conn, reference)?;
        let owner = goal_by_id(conn, &task.goal_id)?;
        let goal = owner.slug.clone();
        let owner_project = project_by_id(conn, &owner.project_id)?;
        let project = project_path(conn, &owner_project)?;
        let links = load_links(conn, &task.id)?;
        let (all_tasks, edges) = load_graph(conn)?;
        let dag = Dag::new(&all_tasks, &edges)?;
        let ids = |iter: &mut dyn Iterator<Item = &Task>| {
            iter.map(|task| task.id.clone()).collect::<Vec<_>>()
        };
        let ready = dag.is_ready(&task);
        let depends_on = ids(&mut dag.dependencies(&task.id));
        let blocked_by = ids(&mut dag.blockers(&task.id));
        let dependents = ids(&mut dag.dependents(&task.id));
        Ok(TaskDetail {
            task,
            project,
            goal,
            ready,
            depends_on,
            blocked_by,
            dependents,
            links,
        })
    }

    /// Load the task, evaluate its dependencies, apply a transition, and save it.
    fn update_task(
        &mut self,
        reference: &str,
        apply: impl FnOnce(&mut Task, bool) -> Result<()>,
    ) -> Result<Task> {
        self.conn.immediate_transaction(|conn| {
            let mut task = find_task(conn, reference)?;
            let (all_tasks, edges) = load_graph(conn)?;
            let dependencies_done = Dag::new(&all_tasks, &edges)?.dependencies_done(&task.id)?;
            apply(&mut task, dependencies_done)?;
            save_task(conn, &task)?;
            Ok(task)
        })
    }

    /// Replace the title of an open task.
    ///
    /// # Errors
    /// Returns an error if the task cannot be resolved, is closed, or the title is blank.
    pub fn set_title(&mut self, reference: &str, title: String) -> Result<Task> {
        self.update_task(reference, |task, _| Ok(task.set_title(title, now())?))
    }

    /// Replace the body of an open task.
    ///
    /// # Errors
    /// Returns an error if the task cannot be resolved or is closed.
    pub fn set_body(&mut self, reference: &str, body: String) -> Result<Task> {
        self.update_task(reference, |task, _| Ok(task.set_body(body, now())?))
    }

    /// Replace the test plan of an open task.
    ///
    /// # Errors
    /// Returns an error if the task cannot be resolved or is closed.
    pub fn set_test_plan(&mut self, reference: &str, test_plan: String) -> Result<Task> {
        self.update_task(reference, |task, _| {
            Ok(task.set_test_plan(test_plan, now())?)
        })
    }

    /// Set or clear the pull request link of an open task.
    ///
    /// # Errors
    /// Returns an error if the task cannot be resolved or is closed.
    pub fn set_pr(&mut self, reference: &str, pr: Option<String>) -> Result<Task> {
        self.update_task(reference, |task, _| Ok(task.set_pr(pr, now())?))
    }

    /// Move a ready task to in progress.
    ///
    /// # Errors
    /// Returns an error if the task cannot be resolved, is not todo, or is blocked.
    pub fn start_task(&mut self, reference: &str) -> Result<Task> {
        self.update_task(reference, |task, done| Ok(task.start(done, now())?))
    }

    /// Move an in-progress task to testing.
    ///
    /// # Errors
    /// Returns an error if the task cannot be resolved or is not in progress.
    pub fn test_task(&mut self, reference: &str) -> Result<Task> {
        self.update_task(reference, |task, _| Ok(task.begin_testing(now())?))
    }

    /// Send a testing task back to in progress.
    ///
    /// # Errors
    /// Returns an error if the task cannot be resolved or is not testing.
    pub fn fail_task(&mut self, reference: &str) -> Result<Task> {
        self.update_task(reference, |task, _| Ok(task.fail_testing(now())?))
    }

    /// Move a testing task to ready for merge.
    ///
    /// # Errors
    /// Returns an error if the task cannot be resolved or is not testing.
    pub fn pass_task(&mut self, reference: &str) -> Result<Task> {
        self.update_task(reference, |task, _| Ok(task.pass_testing(now())?))
    }

    /// Mark a task that is ready for merge done.
    ///
    /// # Errors
    /// Returns an error if the task cannot be resolved or is not ready for merge.
    pub fn finish_task(&mut self, reference: &str) -> Result<Task> {
        self.update_task(reference, |task, _| Ok(task.finish(now())?))
    }

    /// Cancel an open task.
    ///
    /// # Errors
    /// Returns an error if the task cannot be resolved or is already closed.
    pub fn cancel_task(&mut self, reference: &str) -> Result<Task> {
        self.update_task(reference, |task, _| Ok(task.cancel(now())?))
    }

    /// Require `depends_on` to finish before `task` can start. Adding an existing edge is a no-op.
    ///
    /// # Errors
    /// Returns an error if either task cannot be resolved, the tasks belong to different
    /// root projects, the task has started, or the edge would create a cycle.
    pub fn add_dependency(&mut self, task: &str, depends_on: &str) -> Result<Dependency> {
        self.conn.immediate_transaction(|conn| {
            let task = find_task(conn, task)?;
            let depends_on = find_task(conn, depends_on)?;
            let task_project = goal_by_id(conn, &task.goal_id)?.project_id;
            let dependency_project = goal_by_id(conn, &depends_on.goal_id)?.project_id;
            ensure!(
                root_project_id(conn, &task_project)?
                    == root_project_id(conn, &dependency_project)?,
                Error::Invalid(format!(
                    "task {} cannot depend on {}: they belong to different projects",
                    task.id, depends_on.id
                ))
            );
            let (all_tasks, edges) = load_graph(conn)?;
            Dag::new(&all_tasks, &edges)?.check_new_dependency(&task.id, &depends_on.id)?;
            let dependency = Dependency::new(task.id, depends_on.id)?;
            diesel::insert_into(task_dependencies::table)
                .values(DependencyRow::from(&dependency))
                .on_conflict_do_nothing()
                .execute(conn)?;
            Ok(dependency)
        })
    }

    /// Drop a dependency from a task that has not started. Removing a missing edge is a no-op.
    ///
    /// # Errors
    /// Returns an error if either task cannot be resolved or the task has started.
    pub fn remove_dependency(&mut self, task: &str, depends_on: &str) -> Result<()> {
        self.conn.immediate_transaction(|conn| {
            let task = find_task(conn, task)?;
            let depends_on = find_task(conn, depends_on)?;
            task.require_editable_dependencies()?;
            diesel::delete(task_dependencies::table.find((task.id, depends_on.id)))
                .execute(conn)?;
            Ok(())
        })
    }

    /// Todo tasks whose dependencies are all done, within the scope.
    ///
    /// # Errors
    /// Returns an error if a reference cannot be resolved or the stored graph is invalid.
    pub fn ready_tasks(&mut self, scope: &TaskScope) -> Result<Vec<Task>> {
        self.graph_view(scope, |dag| dag.ready().cloned().collect())
    }

    /// Every task in dependency order, prerequisites first, within the scope.
    ///
    /// # Errors
    /// Returns an error if a reference cannot be resolved or the stored graph is invalid.
    pub fn task_order(&mut self, scope: &TaskScope) -> Result<Vec<Task>> {
        self.graph_view(scope, |dag| {
            dag.topological_order().into_iter().cloned().collect()
        })
    }

    fn graph_view(
        &mut self,
        scope: &TaskScope,
        select: impl FnOnce(&Dag) -> Vec<Task>,
    ) -> Result<Vec<Task>> {
        let conn = &mut self.conn;
        let keep = scope_goals(conn, scope)?;
        let (all_tasks, edges) = load_graph(conn)?;
        let dag = Dag::new(&all_tasks, &edges)?;
        Ok(select(&dag)
            .into_iter()
            .filter(|task| keep.as_ref().is_none_or(|ids| ids.contains(&task.goal_id)))
            .collect())
    }

    /// Attach a commit or URL to a task. Re-adding the same link is a no-op.
    ///
    /// # Errors
    /// Returns an error if the task cannot be resolved or the reference is malformed.
    pub fn add_link(&mut self, task: &str, kind: LinkKind, reference: String) -> Result<TaskLink> {
        self.conn.immediate_transaction(|conn| {
            let task = find_task(conn, task)?;
            let link = TaskLink::new(new_id(), task.id, kind, reference, now())?;
            diesel::insert_into(task_links::table)
                .values(LinkRow::from(&link))
                .on_conflict_do_nothing()
                .execute(conn)?;
            Ok(link)
        })
    }

    /// Links attached to a task, oldest first.
    ///
    /// # Errors
    /// Returns an error if the task cannot be resolved.
    pub fn links(&mut self, task: &str) -> Result<Vec<TaskLink>> {
        let conn = &mut self.conn;
        let task = find_task(conn, task)?;
        load_links(conn, &task.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tasky_core::GoalStatus;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::init(&dir.path().join("tasky.db")).unwrap();
        store
            .create_project(None, "app".into(), "App".into(), None, None)
            .unwrap();
        (dir, store)
    }

    fn ids(tasks: &[Task]) -> Vec<&str> {
        tasks.iter().map(|task| task.id.as_str()).collect()
    }

    fn goal(store: &mut Store, project: &str, slug: &str) -> Goal {
        store
            .create_goal(
                project,
                None,
                slug.into(),
                slug.to_uppercase(),
                String::new(),
                None,
            )
            .unwrap()
    }

    fn task(store: &mut Store, goal: &str, title: &str) -> Task {
        store
            .create_task(goal, title.into(), String::new(), String::new())
            .unwrap()
    }

    /// Walk a todo task through the whole lifecycle to done.
    fn finish(store: &mut Store, id: &str) {
        store.start_task(id).unwrap();
        store.test_task(id).unwrap();
        store.pass_task(id).unwrap();
        store.finish_task(id).unwrap();
    }

    fn scope(project: Option<&str>, goal: Option<&str>) -> TaskScope {
        TaskScope {
            project: project.map(str::to_owned),
            goal: goal.map(str::to_owned),
        }
    }

    #[test]
    fn init_creates_an_empty_database_and_refuses_to_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("tasky.db");
        let mut store = Store::init(&path).unwrap();
        assert!(store.projects().unwrap().is_empty());
        assert!(Store::init(&path).is_err());
        assert!(Store::open(&dir.path().join("missing.db")).is_err());
        assert!(Store::open(&path).is_ok());
        assert!(Store::migrate(&path).unwrap().is_empty());
    }

    #[test]
    fn default_path_follows_xdg() {
        let path = default_path();
        assert!(path.ends_with("tasky/tasky.db"), "{}", path.display());
    }

    #[test]
    fn open_never_migrates_but_migrate_does() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.db");
        SqliteConnection::establish(path.to_str().unwrap()).unwrap();
        let error = match Store::open(&path) {
            Ok(_) => panic!("unmigrated database opened"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("tasky migrate"), "{error}");
        assert!(Store::migrate(&dir.path().join("missing.db")).is_err());
        let applied = Store::migrate(&path).unwrap();
        assert_eq!(applied.len(), 1);
        assert!(Store::migrate(&path).unwrap().is_empty());
        assert!(Store::open(&path).is_ok());
    }

    #[test]
    fn projects_are_unique_and_resolvable() {
        let (_dir, mut store) = store();
        let web = store
            .create_project(
                None,
                "web".into(),
                "Web".into(),
                None,
                Some("https://example.com/web.git".into()),
            )
            .unwrap();
        assert_eq!(web.repo_path, None);
        assert_eq!(web.repo_url.as_deref(), Some("https://example.com/web.git"));
        assert!(
            store
                .create_project(None, "web".into(), "Again".into(), None, None)
                .is_err()
        );
        assert!(
            store
                .create_project(None, "Web".into(), "Bad".into(), None, None)
                .is_err()
        );
        assert!(
            store
                .create_project(None, "cli".into(), " ".into(), None, None)
                .is_err()
        );
        assert_eq!(store.projects().unwrap().len(), 2);
        assert_eq!(store.project("web").unwrap().id, web.id);
        let tied = store
            .set_project_repo("app", Change::Set("/home/me/app".into()), Change::Keep)
            .unwrap();
        assert_eq!(tied.repo_path.as_deref(), Some("/home/me/app"));
        assert_eq!(tied.repo_url, None);
        let both = store
            .set_project_repo(
                "app",
                Change::Keep,
                Change::Set("git@example.com:app.git".into()),
            )
            .unwrap();
        assert_eq!(both.repo_path.as_deref(), Some("/home/me/app"), "path kept");
        assert_eq!(both.repo_url.as_deref(), Some("git@example.com:app.git"));
        let cleared = store
            .set_project_repo("app", Change::Clear, Change::Set(" ".into()))
            .unwrap();
        assert_eq!(cleared.repo_path, None);
        assert_eq!(cleared.repo_url, None, "blank clears");
        assert!(
            store
                .set_project_repo("nope", Change::Keep, Change::Keep)
                .is_err()
        );
        assert_eq!(store.project(&web.id[20..]).unwrap().slug, "web");
        assert!(store.project("nope").is_err());
        let detail = store.project_detail("app").unwrap();
        assert_eq!(detail.goals, 0);
        assert_eq!(detail.tasks, TaskCounts::default());
    }

    #[test]
    fn goal_slugs_are_scoped_to_projects() {
        let (_dir, mut store) = store();
        store
            .create_project(None, "web".into(), "Web".into(), None, None)
            .unwrap();
        let app_auth = goal(&mut store, "app", "auth");
        assert!(
            store
                .create_goal(
                    "app",
                    None,
                    "auth".into(),
                    "Again".into(),
                    String::new(),
                    None
                )
                .is_err(),
            "duplicate within project"
        );
        assert!(
            store
                .create_goal("nope", None, "x".into(), "X".into(), String::new(), None)
                .is_err()
        );
        let web_auth = goal(&mut store, "web", "auth");
        assert_ne!(app_auth.id, web_auth.id);

        assert!(store.goal("auth").is_err(), "slug shared by two projects");
        assert_eq!(store.goal("app/auth").unwrap().id, app_auth.id);
        assert_eq!(store.goal("web/auth").unwrap().id, web_auth.id);
        assert!(store.goal("web/nope").is_err());
        assert!(store.goal("nope/auth").is_err());
        assert_eq!(store.goal(&app_auth.id[20..]).unwrap().id, app_auth.id);
        assert_eq!(
            store
                .goal(&web_auth.id[20..].to_ascii_lowercase())
                .unwrap()
                .id,
            web_auth.id
        );

        goal(&mut store, "app", "billing");
        assert_eq!(
            store.goal("billing").unwrap().project_id,
            app_auth.project_id
        );
        assert_eq!(store.goals(None).unwrap().len(), 3);
        assert_eq!(store.goals(Some("web")).unwrap().len(), 1);
        assert_eq!(store.project_detail("app").unwrap().goals, 2);
        assert_eq!(store.goal_detail("web/auth").unwrap().project, "web");
    }

    #[test]
    fn goal_lifecycle_and_optional_spec() {
        let (_dir, mut store) = store();
        let created = store
            .create_goal(
                "app",
                None,
                "auth".into(),
                "Auth".into(),
                "Log in".into(),
                Some("# Auth\n".into()),
            )
            .unwrap();
        assert_eq!(created.spec.as_deref(), Some("# Auth\n"));
        let cleared = store.set_goal_spec("auth", Some("  ".into())).unwrap();
        assert_eq!(cleared.spec, None);
        store.activate_goal("auth").unwrap();
        assert!(store.activate_goal("auth").is_err());
        let updated = store
            .set_goal_spec("auth", Some("Requirements".into()))
            .unwrap();
        assert_eq!(updated.spec.as_deref(), Some("Requirements"));

        let task = task(&mut store, "auth", "Write login");
        assert_eq!(store.goal_detail("auth").unwrap().tasks.todo, 1);
        assert!(store.complete_goal("auth").is_err(), "open task");
        finish(&mut store, &task.id);
        let done = store.complete_goal("auth").unwrap();
        assert_eq!(done.status, GoalStatus::Complete);
        assert!(done.completed_at.is_some());
        assert!(
            store
                .create_task("auth", "Late".into(), String::new(), String::new())
                .is_err()
        );
        assert!(store.set_goal_spec("auth", None).is_err());
        assert!(store.cancel_goal("auth").is_err());
        assert_eq!(
            store.goal("auth").unwrap().spec.as_deref(),
            Some("Requirements")
        );

        goal(&mut store, "app", "draft");
        assert!(store.complete_goal("draft").is_err(), "not active");
        assert_eq!(
            store.cancel_goal("draft").unwrap().status,
            GoalStatus::Cancelled
        );
    }

    #[test]
    fn dependencies_gate_work_and_reject_cycles() {
        let (_dir, mut store) = store();
        goal(&mut store, "app", "api");
        goal(&mut store, "app", "ui");
        let design = task(&mut store, "api", "Design");
        let build = task(&mut store, "api", "Build");
        let screen = task(&mut store, "ui", "Screen");
        store.add_dependency(&build.id, &design.id).unwrap();
        store.add_dependency(&build.id, &design.id).unwrap();
        store.add_dependency(&screen.id, &build.id).unwrap();
        assert!(
            store.add_dependency(&design.id, &screen.id).is_err(),
            "cycle"
        );
        assert!(store.add_dependency(&design.id, &design.id).is_err());
        assert!(store.add_dependency(&design.id, "zzz").is_err());
        assert!(store.task("0").is_err(), "shared prefix is ambiguous");
        assert_eq!(store.task(&design.id[20..]).unwrap().id, design.id);
        assert!(store.task("not-an-id").is_err());

        let all = scope(None, None);
        assert_eq!(
            ids(&store.ready_tasks(&all).unwrap()),
            vec![design.id.as_str()]
        );
        assert!(
            store
                .ready_tasks(&scope(None, Some("ui")))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            ids(&store.task_order(&all).unwrap()),
            vec![design.id.as_str(), build.id.as_str(), screen.id.as_str()]
        );
        assert_eq!(
            ids(&store.task_order(&scope(Some("app"), Some("ui"))).unwrap()),
            vec![screen.id.as_str()]
        );
        assert_eq!(
            store.task_order(&scope(Some("app"), None)).unwrap().len(),
            3
        );

        assert!(store.start_task(&build.id).is_err(), "blocked");
        let detail = store.task_detail(&build.id).unwrap();
        assert_eq!(detail.project, "app");
        assert_eq!(detail.goal, "api");
        assert!(!detail.ready);
        assert_eq!(detail.blocked_by, vec![design.id.clone()]);
        assert_eq!(detail.dependents, vec![screen.id.clone()]);

        store.start_task(&design.id).unwrap();
        assert!(
            store.add_dependency(&design.id, &build.id).is_err(),
            "started"
        );
        assert!(store.remove_dependency(&design.id, &build.id).is_err());
        assert!(store.finish_task(&design.id).is_err(), "must test first");
        store.test_task(&design.id).unwrap();
        assert_eq!(store.task(&design.id).unwrap().status, TaskStatus::Testing);
        assert!(store.ready_tasks(&all).unwrap().is_empty(), "still blocked");
        store.fail_task(&design.id).unwrap();
        store.test_task(&design.id).unwrap();
        store.pass_task(&design.id).unwrap();
        assert!(store.ready_tasks(&all).unwrap().is_empty(), "still blocked");
        store.finish_task(&design.id).unwrap();
        assert_eq!(
            ids(&store.ready_tasks(&all).unwrap()),
            vec![build.id.as_str()]
        );
        store.remove_dependency(&screen.id, &build.id).unwrap();
        assert_eq!(store.ready_tasks(&all).unwrap().len(), 2);
        store.cancel_task(&screen.id).unwrap();
        assert!(store.finish_task(&screen.id).is_err());

        let filter = TaskFilter {
            scope: scope(None, Some("api")),
            status: Some(TaskStatus::Done),
        };
        assert_eq!(
            ids(&store.tasks(&filter).unwrap()),
            vec![design.id.as_str()]
        );
        assert_eq!(store.tasks(&TaskFilter::default()).unwrap().len(), 3);
        assert_eq!(store.project_detail("app").unwrap().tasks.done, 1);

        store
            .add_link(&build.id, LinkKind::Commit, "abc123".into())
            .unwrap();
        store
            .add_link(&build.id, LinkKind::Commit, "abc123".into())
            .unwrap();
        assert!(
            store
                .add_link(&build.id, LinkKind::Url, " ".into())
                .is_err()
        );
        assert_eq!(store.links(&build.id).unwrap().len(), 1);
        assert_eq!(store.task_detail(&build.id).unwrap().links.len(), 1);
    }

    #[test]
    fn title_body_test_plan_and_pr_live_on_the_task() {
        let (_dir, mut store) = store();
        goal(&mut store, "app", "f");
        let task = store
            .create_task("f", "Work".into(), String::new(), "cargo test".into())
            .unwrap();
        assert_eq!(task.test_plan, "cargo test");
        assert_eq!(task.pr, None);
        let with_title = store.set_title(&task.id, "Work harder".into()).unwrap();
        assert_eq!(with_title.title, "Work harder");
        let with_body = store.set_body(&task.id, "Build it.".into()).unwrap();
        assert_eq!(with_body.body, "Build it.");
        let updated = store
            .set_test_plan(&task.id, "1. cargo test".into())
            .unwrap();
        assert_eq!(updated.test_plan, "1. cargo test");
        let with_pr = store
            .set_pr(&task.id, Some("https://example.com/pr/7".into()))
            .unwrap();
        assert_eq!(with_pr.pr.as_deref(), Some("https://example.com/pr/7"));
        finish(&mut store, &task.id);
        let done = store.task(&task.id).unwrap();
        assert_eq!(done.test_plan, "1. cargo test");
        assert_eq!(done.pr.as_deref(), Some("https://example.com/pr/7"));
        assert!(store.set_pr(&task.id, None).is_err(), "done is frozen");
        assert!(store.set_test_plan(&task.id, "x".into()).is_err());
        assert!(store.set_body(&task.id, "x".into()).is_err());
        assert!(store.set_title(&task.id, "x".into()).is_err());
        assert_eq!(done.title, "Work harder");
        assert_eq!(done.body, "Build it.");
        assert_eq!(store.goal_detail("f").unwrap().tasks.done, 1);
    }

    #[test]
    fn projects_are_isolated() {
        let (_dir, mut store) = store();
        store
            .create_project(None, "web".into(), "Web".into(), None, None)
            .unwrap();
        goal(&mut store, "app", "core");
        goal(&mut store, "web", "site");
        let app_task = task(&mut store, "core", "App work");
        let web_task = task(&mut store, "site", "Web work");
        assert!(
            store.add_dependency(&web_task.id, &app_task.id).is_err(),
            "edges never cross projects"
        );
        assert_eq!(
            ids(&store.ready_tasks(&scope(Some("web"), None)).unwrap()),
            vec![web_task.id.as_str()]
        );
        assert!(
            store
                .ready_tasks(&scope(Some("web"), Some("core")))
                .is_err(),
            "goal is in another project"
        );
        assert_eq!(store.ready_tasks(&scope(None, None)).unwrap().len(), 2);
    }

    #[test]
    fn competing_connections_cannot_both_start_a_task() {
        let (dir, mut store) = store();
        let path = dir.path().join("tasky.db");
        goal(&mut store, "app", "f");
        let task = task(&mut store, "f", "Work");
        let results = std::thread::scope(|scope| {
            let workers = [(); 4]
                .map(|()| scope.spawn(|| Store::open(&path).unwrap().start_task(&task.id).is_ok()));
            workers.map(|worker| worker.join().unwrap())
        });
        assert_eq!(results.iter().filter(|ok| **ok).count(), 1);
        assert_eq!(store.task(&task.id).unwrap().status, TaskStatus::InProgress);
    }

    #[test]
    fn projects_nest_by_path_and_slugs_are_unique_among_siblings() {
        let (_dir, mut store) = store();
        let mobile = store
            .create_project(Some("app"), "mobile".into(), "Mobile".into(), None, None)
            .unwrap();
        let ios = store
            .create_project(Some("app/mobile"), "ios".into(), "iOS".into(), None, None)
            .unwrap();
        assert_eq!(
            mobile.parent_id.as_deref(),
            Some(store.project("app").unwrap().id.as_str())
        );
        assert_eq!(store.project("app/mobile/ios").unwrap().id, ios.id);
        assert!(store.project("app/ios").is_err(), "paths must be exact");
        assert!(store.project("mobile").is_err(), "a child is not a root");
        assert!(
            store
                .create_project(Some("app"), "mobile".into(), "Again".into(), None, None)
                .is_err(),
            "duplicate among siblings"
        );
        store
            .create_project(None, "mobile".into(), "Root mobile".into(), None, None)
            .unwrap();
        assert!(
            store
                .create_project(None, "mobile".into(), "Again".into(), None, None)
                .is_err(),
            "duplicate root"
        );
        assert!(
            store
                .create_project(Some("nope"), "x".into(), "X".into(), None, None)
                .is_err()
        );
        let detail = store.project_detail("app").unwrap();
        assert_eq!(detail.path, "app");
        assert_eq!(detail.subprojects, 1);
        assert_eq!(
            store.project_detail("app/mobile/ios").unwrap().path,
            "app/mobile/ios"
        );
        assert_eq!(store.projects().unwrap().len(), 4);
    }

    #[test]
    fn subgoals_stay_in_their_project_and_gate_closing() {
        let (_dir, mut store) = store();
        store
            .create_project(None, "web".into(), "Web".into(), None, None)
            .unwrap();
        goal(&mut store, "app", "auth");
        goal(&mut store, "web", "site");
        let login = store
            .create_goal(
                "app",
                Some("app/auth"),
                "login".into(),
                "Login".into(),
                String::new(),
                None,
            )
            .unwrap();
        assert_eq!(login.parent_id, Some(store.goal("app/auth").unwrap().id));
        assert!(
            store
                .create_goal(
                    "app",
                    Some("web/site"),
                    "x".into(),
                    "X".into(),
                    String::new(),
                    None
                )
                .is_err(),
            "parent in another project"
        );
        assert!(
            store
                .create_goal(
                    "app",
                    Some("app/auth"),
                    "auth".into(),
                    "Dup".into(),
                    String::new(),
                    None
                )
                .is_err(),
            "slugs stay unique per project at any depth"
        );
        let work = task(&mut store, "app/login", "Build login");
        store.activate_goal("app/auth").unwrap();
        store.activate_goal("app/login").unwrap();
        assert_eq!(store.goal_detail("app/auth").unwrap().subgoals, 1);
        assert_eq!(
            store.goal_detail("app/auth").unwrap().tasks.todo,
            1,
            "subtree counts"
        );
        assert!(
            store.complete_goal("app/auth").is_err(),
            "sub-goal still open"
        );
        assert!(store.cancel_goal("app/auth").is_err(), "cancel waits too");
        assert!(
            store.complete_goal("app/login").is_err(),
            "its task is open"
        );
        finish(&mut store, &work.id);
        store.complete_goal("app/login").unwrap();
        assert_eq!(
            store.complete_goal("app/auth").unwrap().status,
            GoalStatus::Complete
        );
        assert!(
            store
                .create_goal(
                    "app",
                    Some("app/auth"),
                    "late".into(),
                    "Late".into(),
                    String::new(),
                    None
                )
                .is_err(),
            "closed goals take no new sub-goals"
        );
        assert_eq!(store.goals(Some("app")).unwrap().len(), 2);
    }

    #[test]
    fn scopes_and_dependencies_follow_the_project_tree() {
        let (_dir, mut store) = store();
        store
            .create_project(Some("app"), "mobile".into(), "Mobile".into(), None, None)
            .unwrap();
        store
            .create_project(None, "other".into(), "Other".into(), None, None)
            .unwrap();
        goal(&mut store, "app", "core");
        goal(&mut store, "app/mobile", "ui");
        goal(&mut store, "other", "misc");
        let core = task(&mut store, "app/core", "Core");
        let ui = task(&mut store, "app/mobile/ui", "UI");
        let misc = task(&mut store, "other/misc", "Misc");
        store.add_dependency(&ui.id, &core.id).unwrap();
        assert!(
            store.add_dependency(&misc.id, &core.id).is_err(),
            "different root projects"
        );
        assert_eq!(
            store.goals(Some("app")).unwrap().len(),
            2,
            "goals of sub-projects too"
        );
        assert_eq!(store.goals(Some("app/mobile")).unwrap().len(), 1);
        assert_eq!(
            store.task_order(&scope(Some("app"), None)).unwrap().len(),
            2,
            "project scope covers sub-projects"
        );
        assert_eq!(
            store
                .task_order(&scope(Some("app/mobile"), None))
                .unwrap()
                .len(),
            1
        );
        assert!(
            store
                .ready_tasks(&scope(Some("app/mobile"), Some("app/core")))
                .is_err(),
            "goal outside the project subtree"
        );
        assert_eq!(store.project_detail("app").unwrap().tasks.todo, 2);
        assert_eq!(store.task_detail(&ui.id).unwrap().project, "app/mobile");
    }
}
