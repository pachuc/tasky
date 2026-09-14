use crate::Error;
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

macro_rules! text_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The stable text form used in storage and on the command line.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = Error;

            fn from_str(value: &str) -> Result<Self, Error> {
                match value {
                    $($text => Ok(Self::$variant),)+
                    other => Err(Error::Invalid(format!(
                        "unknown {}: {other:?}",
                        stringify!($name)
                    ))),
                }
            }
        }
    };
}

text_enum! {
    /// Lifecycle of a goal. Completion is explicit and requires finished tasks.
    GoalStatus {
        Draft => "draft",
        Active => "active",
        Complete => "complete",
        Cancelled => "cancelled",
    }
}

text_enum! {
    /// Lifecycle of a task: `todo → in_progress ⇄ testing → ready_for_merge → done`, or
    /// `cancelled` from any open state. "Blocked" is derived from dependencies, never stored.
    TaskStatus {
        Todo => "todo",
        InProgress => "in_progress",
        Testing => "testing",
        ReadyForMerge => "ready_for_merge",
        Done => "done",
        Cancelled => "cancelled",
    }
}

text_enum! {
    /// What an external reference attached to a task points at.
    LinkKind {
        Commit => "commit",
        Url => "url",
    }
}

impl GoalStatus {
    /// Whether the goal is finished for good: complete or cancelled.
    #[must_use]
    pub const fn is_closed(self) -> bool {
        matches!(self, Self::Complete | Self::Cancelled)
    }
}

impl TaskStatus {
    /// Whether the task still has work outstanding, that is, it is neither done nor cancelled.
    #[must_use]
    pub const fn is_open(self) -> bool {
        !matches!(self, Self::Done | Self::Cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_forms_round_trip() {
        for status in TaskStatus::ALL {
            assert_eq!(status.as_str().parse::<TaskStatus>().unwrap(), *status);
        }
        for status in GoalStatus::ALL {
            assert_eq!(status.as_str().parse::<GoalStatus>().unwrap(), *status);
        }
        assert!("bogus".parse::<TaskStatus>().is_err());
        assert_eq!("commit".parse::<LinkKind>().unwrap(), LinkKind::Commit);
        assert_eq!(
            "ready_for_merge".parse::<TaskStatus>().unwrap(),
            TaskStatus::ReadyForMerge
        );
        assert!(TaskStatus::Testing.is_open());
        assert!(!TaskStatus::Cancelled.is_open());
    }
}
