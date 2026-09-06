use crate::workspace::Workspace;

use super::WorkflowError;

/// Pipeline mode for shipping work: open a PR or apply locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShipMode {
    Pr,
    Local,
}

impl ShipMode {
    pub fn as_input_value(self) -> &'static str {
        match self {
            Self::Pr => "pr",
            Self::Local => "local",
        }
    }

    pub fn parse(value: &str) -> Result<Self, WorkflowError> {
        match value {
            "pr" => Ok(Self::Pr),
            "local" => Ok(Self::Local),
            other => Err(WorkflowError::Invalid(format!(
                "unknown ship mode '{other}' (expected 'pr' or 'local')"
            ))),
        }
    }
}

/// Resolve a workspace's effective ship mode, defaulting safely to PR mode.
pub fn resolved_ship_mode(workspace: &Workspace) -> ShipMode {
    workspace
        .ship_mode
        .as_deref()
        .and_then(|value| ShipMode::parse(value).ok())
        .unwrap_or(ShipMode::Pr)
}

/// How far a submitted run is authorized to carry the work it delivers.
///
/// `Review` is the default everywhere: a successful task ends in `review` and a
/// separate operator action completes it. `Done` is explicit operator
/// authorization, granted per invocation (`--complete`), for the run to finish
/// delivery and perform the guarded `review -> done` transition. It is never
/// derived from workspace configuration or the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompletionPolicy {
    #[default]
    Review,
    Done,
}

impl CompletionPolicy {
    pub fn as_input_value(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Done => "done",
        }
    }

    /// Whether this policy authorizes the `review -> done` transition.
    pub fn completes(self) -> bool {
        matches!(self, Self::Done)
    }

    pub fn parse(value: &str) -> Result<Self, WorkflowError> {
        match value {
            "review" => Ok(Self::Review),
            "done" => Ok(Self::Done),
            other => Err(WorkflowError::Invalid(format!(
                "unknown completion policy '{other}' (expected 'review' or 'done')"
            ))),
        }
    }
}
