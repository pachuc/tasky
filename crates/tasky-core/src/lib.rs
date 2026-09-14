//! Domain model and rules for Tasky. No filesystem, database, clock, or process execution.
//!
//! One database holds many projects. A project is anything work is organized under and may
//! optionally point at a repository. A project contains goals, and every task belongs to a
//! goal. Tasks depend on other tasks in the same project, forming a DAG. Callers supply the
//! current time and generated IDs; this crate only enforces the rules.

mod dag;
mod goal;
mod project;
mod status;
mod task;

pub use dag::Dag;
pub use goal::Goal;
pub use jiff::Timestamp;
pub use project::Project;
pub use status::{GoalStatus, LinkKind, TaskStatus};
pub use task::{Dependency, Task, TaskLink};

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("ambiguous reference: {0}")]
    Ambiguous(String),
    #[error("already exists: {0}")]
    Duplicate(String),
    #[error("invalid operation: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;

fn require_nonblank(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::Invalid(format!("{field} must not be blank")));
    }
    Ok(())
}

/// Slugs are what humans type, so keep them shell- and URL-safe. A slug never contains `/`,
/// which lets `project/goal` references be split unambiguously.
fn validate_slug(slug: &str) -> Result<()> {
    let valid = !slug.is_empty()
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !valid {
        return Err(Error::Invalid(format!(
            "slug {slug:?} must be lowercase letters, digits, and inner hyphens"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_slug;

    #[test]
    fn slugs_are_validated() {
        for bad in [
            "",
            "Auth",
            "-auth",
            "auth-",
            "auth login",
            "auth_login",
            "a/b",
        ] {
            assert!(validate_slug(bad).is_err(), "{bad:?} should be rejected");
        }
        assert!(validate_slug("auth-2").is_ok());
    }
}
