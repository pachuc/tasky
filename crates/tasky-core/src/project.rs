use crate::{Result, Timestamp, require_nonblank, validate_slug};
use serde::{Deserialize, Serialize};

/// Anything work is organized under: a codebase, a research effort, a launch. One database
/// holds any number of projects. A coding project may point at its repository on disk, on a
/// remote, or both; nothing requires either.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub slug: String,
    pub name: String,
    /// Local checkout of the repository, when there is one.
    pub repo_path: Option<String>,
    /// Remote URL of the repository, when there is one.
    pub repo_url: Option<String>,
    pub created_at: Timestamp,
}

/// Blank text means "not set", so callers cannot store whitespace by accident.
fn normalize(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.trim().is_empty())
}

impl Project {
    /// Describe a project.
    ///
    /// # Errors
    /// Returns an error if the ID or name is blank or the slug is malformed.
    pub fn new(
        id: String,
        slug: String,
        name: String,
        repo_path: Option<String>,
        repo_url: Option<String>,
        now: Timestamp,
    ) -> Result<Self> {
        require_nonblank("id", &id)?;
        validate_slug(&slug)?;
        require_nonblank("project name", &name)?;
        Ok(Self {
            id,
            slug,
            name,
            repo_path: normalize(repo_path),
            repo_url: normalize(repo_url),
            created_at: now,
        })
    }

    /// Set or clear the local checkout of the repository.
    pub fn set_repo_path(&mut self, path: Option<String>) {
        self.repo_path = normalize(path);
    }

    /// Set or clear the remote URL of the repository.
    pub fn set_repo_url(&mut self, url: Option<String>) {
        self.repo_url = normalize(url);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_need_a_slug_and_name() {
        let now = Timestamp::UNIX_EPOCH;
        let new = |slug: &str, name: &str| {
            Project::new("P".into(), slug.into(), name.into(), None, None, now)
        };
        assert!(new("tasky", "Tasky").is_ok());
        assert!(new("Tasky", "Tasky").is_err());
        assert!(new("tasky", " ").is_err());
    }

    #[test]
    fn repo_path_and_url_are_independent_and_optional() {
        let now = Timestamp::UNIX_EPOCH;
        let mut project = Project::new(
            "P".into(),
            "tasky".into(),
            "Tasky".into(),
            Some(" ".into()),
            Some("git@example.com:tasky.git".into()),
            now,
        )
        .unwrap();
        assert_eq!(project.repo_path, None, "blank is dropped");
        assert_eq!(
            project.repo_url.as_deref(),
            Some("git@example.com:tasky.git")
        );
        project.set_repo_path(Some("/home/me/tasky".into()));
        project.set_repo_url(None);
        assert_eq!(project.repo_path.as_deref(), Some("/home/me/tasky"));
        assert_eq!(project.repo_url, None);
    }
}
