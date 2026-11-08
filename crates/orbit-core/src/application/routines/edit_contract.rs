//! The one result and refusal contract of a routine definition edit (toggle
//! and trigger edit). Every surface — `orbit routine enable|disable`,
//! `orbit.routine.control` and the dashboard — renders these types; none
//! decides on its own which code, message or observed value a refusal carries.

use orbit_common::OrbitError;
use orbit_types::workflow::{RoutineTarget, RoutineTrigger};
use serde_json::{Map, Value, json};

/// A routine definition edit that ran to completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutineEditOutcome {
    /// `true` when the definition file was rewritten; `false` when the
    /// requested value already matched it.
    pub changed: bool,
}

/// Why a routine definition edit wrote nothing. Each refusal has a stable
/// [`code`](Self::code) and one operator-facing [`message`](Self::message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutineEditRefusal {
    /// The report lists no schedulable routine of that name.
    NotFound {
        /// The requested routine name.
        name: String,
    },
    /// The routine comes from another source workspace than the caller's.
    WorkspaceMismatch {
        /// The requested routine name.
        name: String,
        /// The routine's actual source workspace name.
        source_workspace: String,
    },
    /// A replica lists the routine, but only its owner may change it.
    OwnerAuthority {
        /// Operator-facing explanation naming the owner.
        reason: String,
        /// The machine that owns the workspace's coordination store.
        owner_machine: String,
    },
    /// The caller's expected `enabled` state was stale.
    StaleState {
        /// The `enabled` value the definition holds now.
        actual_enabled: bool,
    },
    /// The caller's expected trigger was stale.
    StaleTrigger {
        /// The trigger the definition holds now.
        actual_trigger: Box<RoutineTrigger>,
    },
    /// The definition now dispatches a different target than the caller
    /// selected.
    TargetMismatch {
        /// The target the definition holds now.
        actual_target: RoutineTarget,
    },
}

impl RoutineEditRefusal {
    /// The stable refusal code every surface reports.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "routine_not_found",
            Self::WorkspaceMismatch { .. } => "workspace_mismatch",
            Self::OwnerAuthority { .. } => "owner_authority",
            Self::StaleState { .. } => "stale_routine_state",
            Self::StaleTrigger { .. } => "stale_routine_trigger",
            Self::TargetMismatch { .. } => "target_mismatch",
        }
    }

    /// The operator-facing explanation, without the code.
    pub fn message(&self) -> String {
        match self {
            Self::NotFound { name } => format!("routine '{name}' was not found"),
            Self::WorkspaceMismatch {
                name,
                source_workspace,
            } => format!(
                "select routine source workspace '{source_workspace}' before changing '{name}'"
            ),
            Self::OwnerAuthority { reason, .. } => reason.clone(),
            Self::StaleState { .. } => {
                "routine state changed while this action was pending; refresh before retrying"
                    .to_string()
            }
            Self::StaleTrigger { .. } => {
                "routine trigger changed while this edit was open; refresh before retrying"
                    .to_string()
            }
            Self::TargetMismatch { actual_target } => format!(
                "routine target changed; refresh and confirm '{}'",
                actual_target.as_ref_string()
            ),
        }
    }

    /// The refusal as a JSON body: `error` (the message), `code`, and the
    /// observed value a client refreshes from (`owner_machine`,
    /// `actual_enabled`, `actual_trigger` or `actual_target`).
    pub fn to_json(&self) -> Value {
        let mut body = Map::new();
        body.insert("error".into(), json!(self.message()));
        body.insert("code".into(), json!(self.code()));
        match self {
            Self::NotFound { .. } | Self::WorkspaceMismatch { .. } => {}
            Self::OwnerAuthority { owner_machine, .. } => {
                body.insert("owner_machine".into(), json!(owner_machine));
            }
            Self::StaleState { actual_enabled } => {
                body.insert("actual_enabled".into(), json!(actual_enabled));
            }
            Self::StaleTrigger { actual_trigger } => {
                body.insert("actual_trigger".into(), json!(actual_trigger));
            }
            Self::TargetMismatch { actual_target } => {
                body.insert("actual_target".into(), json!(actual_target.as_ref_string()));
            }
        }
        Value::Object(body)
    }
}

/// The refusal as an error-only surface (CLI, MCP) reports it: owner
/// authority is a capability refusal carrying the owner's reason; every other
/// refusal is invalid input reading `<code>: <message>`.
impl From<RoutineEditRefusal> for OrbitError {
    fn from(refusal: RoutineEditRefusal) -> Self {
        match refusal {
            RoutineEditRefusal::OwnerAuthority { reason, .. } => {
                OrbitError::CapabilityRefused(reason)
            }
            other => OrbitError::InvalidInput(format!("{}: {}", other.code(), other.message())),
        }
    }
}
