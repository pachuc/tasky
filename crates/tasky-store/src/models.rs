//! Row types mirroring `schema.rs`. Conversions to and from domain types validate stored text.

use crate::schema::{goals, projects, task_dependencies, task_links, tasks};
use anyhow::{Context, Result};
use diesel::prelude::*;
use tasky_core::{Dependency, Goal, Project, Task, TaskLink, Timestamp};

fn parse_time(field: &str, value: &str) -> Result<Timestamp> {
    value
        .parse()
        .with_context(|| format!("invalid {field} timestamp {value:?}"))
}

fn parse_optional_time(field: &str, value: Option<&str>) -> Result<Option<Timestamp>> {
    value.map(|value| parse_time(field, value)).transpose()
}

#[derive(Debug, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = projects, treat_none_as_null = true)]
pub struct ProjectRow {
    pub id: String,
    pub parent_id: Option<String>,
    pub slug: String,
    pub name: String,
    pub repo_path: Option<String>,
    pub repo_url: Option<String>,
    pub created_at: String,
}

impl TryFrom<ProjectRow> for Project {
    type Error = anyhow::Error;

    fn try_from(row: ProjectRow) -> Result<Self> {
        Ok(Self {
            created_at: parse_time("created_at", &row.created_at)?,
            id: row.id,
            parent_id: row.parent_id,
            slug: row.slug,
            name: row.name,
            repo_path: row.repo_path,
            repo_url: row.repo_url,
        })
    }
}

impl From<&Project> for ProjectRow {
    fn from(project: &Project) -> Self {
        Self {
            id: project.id.clone(),
            parent_id: project.parent_id.clone(),
            slug: project.slug.clone(),
            name: project.name.clone(),
            repo_path: project.repo_path.clone(),
            repo_url: project.repo_url.clone(),
            created_at: project.created_at.to_string(),
        }
    }
}

#[derive(Debug, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = goals, treat_none_as_null = true)]
pub struct GoalRow {
    pub id: String,
    pub project_id: String,
    pub parent_id: Option<String>,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub spec: Option<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

impl TryFrom<GoalRow> for Goal {
    type Error = anyhow::Error;

    fn try_from(row: GoalRow) -> Result<Self> {
        Ok(Self {
            status: row.status.parse()?,
            created_at: parse_time("created_at", &row.created_at)?,
            updated_at: parse_time("updated_at", &row.updated_at)?,
            completed_at: parse_optional_time("completed_at", row.completed_at.as_deref())?,
            id: row.id,
            project_id: row.project_id,
            parent_id: row.parent_id,
            slug: row.slug,
            title: row.title,
            description: row.description,
            spec: row.spec,
        })
    }
}

impl From<&Goal> for GoalRow {
    fn from(goal: &Goal) -> Self {
        Self {
            id: goal.id.clone(),
            project_id: goal.project_id.clone(),
            parent_id: goal.parent_id.clone(),
            slug: goal.slug.clone(),
            title: goal.title.clone(),
            description: goal.description.clone(),
            spec: goal.spec.clone(),
            status: goal.status.as_str().into(),
            created_at: goal.created_at.to_string(),
            updated_at: goal.updated_at.to_string(),
            completed_at: goal.completed_at.map(|time| time.to_string()),
        }
    }
}

#[derive(Debug, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = tasks, treat_none_as_null = true)]
pub struct TaskRow {
    pub id: String,
    pub goal_id: String,
    pub title: String,
    pub body: String,
    pub test_plan: String,
    pub pr: Option<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

impl TryFrom<TaskRow> for Task {
    type Error = anyhow::Error;

    fn try_from(row: TaskRow) -> Result<Self> {
        Ok(Self {
            status: row.status.parse()?,
            created_at: parse_time("created_at", &row.created_at)?,
            updated_at: parse_time("updated_at", &row.updated_at)?,
            completed_at: parse_optional_time("completed_at", row.completed_at.as_deref())?,
            id: row.id,
            goal_id: row.goal_id,
            title: row.title,
            body: row.body,
            test_plan: row.test_plan,
            pr: row.pr,
        })
    }
}

impl From<&Task> for TaskRow {
    fn from(task: &Task) -> Self {
        Self {
            id: task.id.clone(),
            goal_id: task.goal_id.clone(),
            title: task.title.clone(),
            body: task.body.clone(),
            test_plan: task.test_plan.clone(),
            pr: task.pr.clone(),
            status: task.status.as_str().into(),
            created_at: task.created_at.to_string(),
            updated_at: task.updated_at.to_string(),
            completed_at: task.completed_at.map(|time| time.to_string()),
        }
    }
}

#[derive(Debug, Queryable, Selectable, Insertable)]
#[diesel(table_name = task_dependencies)]
pub struct DependencyRow {
    pub task_id: String,
    pub depends_on_id: String,
}

impl TryFrom<DependencyRow> for Dependency {
    type Error = anyhow::Error;

    fn try_from(row: DependencyRow) -> Result<Self> {
        Ok(Self::new(row.task_id, row.depends_on_id)?)
    }
}

impl From<&Dependency> for DependencyRow {
    fn from(dependency: &Dependency) -> Self {
        Self {
            task_id: dependency.task_id.clone(),
            depends_on_id: dependency.depends_on_id.clone(),
        }
    }
}

#[derive(Debug, Queryable, Selectable, Insertable)]
#[diesel(table_name = task_links)]
pub struct LinkRow {
    pub id: String,
    pub task_id: String,
    pub kind: String,
    pub reference: String,
    pub created_at: String,
}

impl TryFrom<LinkRow> for TaskLink {
    type Error = anyhow::Error;

    fn try_from(row: LinkRow) -> Result<Self> {
        Ok(Self {
            kind: row.kind.parse()?,
            created_at: parse_time("created_at", &row.created_at)?,
            id: row.id,
            task_id: row.task_id,
            reference: row.reference,
        })
    }
}

impl From<&TaskLink> for LinkRow {
    fn from(link: &TaskLink) -> Self {
        Self {
            id: link.id.clone(),
            task_id: link.task_id.clone(),
            kind: link.kind.as_str().into(),
            reference: link.reference.clone(),
            created_at: link.created_at.to_string(),
        }
    }
}
