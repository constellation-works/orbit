//! The resident app: grant, relay to, control and read resident mandates.
//!
//! [`Command::Operation`] is a verb of the resident noun in the operation
//! registry (`orbit_common::governance::resident`) with its fields, named
//! and typed as the verb's spec declares; its [`Outcome`] carries the JSON
//! that spec documents.
//!
//! A registry command from a surface still runs through the generic tool
//! entry, which checks the fields against the spec, decides the tool row and
//! reaches the resident tool handler. The handler calls [`exec`] again from
//! inside that entry, and that call runs the flow.
//!
//! Task decision answers and resident comment admission enter through
//! the task-owned `TaskResidentPort`, bound in `app::port`. A cycle fires
//! through the `resident_due` trigger, runs as `resident_cycle_pipeline`
//! and reaches tools through the cycle guard.
//!
//! The dashboard's commands have no registry verb. Its two reads answer the
//! Residents view's models; each of its writes is decided against the
//! dashboard's resident operation and leaves one `dashboard.operations` row.

mod assignment;

mod comment;

mod control;

mod create;

mod cycle;

mod cycle_tool;

mod dashboard;

mod digest;

mod due;

mod grant;

mod guard;

mod mandate;

mod read;

mod metric;

mod operation;

mod routine_status_port;

mod step;

mod task_port;

mod view;

mod wake;

mod worktree;

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_common::governance::resident::ResidentVerb;
use serde_json::{Map, Value};

use super::exec::{self, App, DashboardOrigin, DashboardRow, Operation};
use super::tool_call;
pub use super::tool_call::Input;
use crate::Core;
pub(crate) use comment::{
    clear_resident_checkpoint_refusal_on_accept, ensure_resident_comment_redaction_safe,
    is_checkpoint_comment_json, record_resident_checkpoint_refusal_if_live,
};
pub(crate) use control::{
    ResidentControlState, hand_off_stopped_resident_at, resident_control_state,
};
pub(crate) use cycle_tool::resident_cycle_granted_tools;
pub(crate) use due::{ResidentDue, resident_due};
pub(crate) use guard::{
    ResidentActionTicket, ResidentCallAdmission, ResidentCallClass, admit_resident_cycle_call,
    clear_failed_resident_action_key, close_resident_action, is_live_resident_cycle_run,
    resident_cycle_attention_scope, resident_cycle_drain_status_allowed,
    resident_recovery_workspace, resident_task_update_audit, resident_workspace_is_local,
};
pub(crate) use mandate::{RESIDENT_CYCLE_ROUTINE_TARGET, ResidentMandateState};
use mandate::{resident_cycle_routine_disabled_reason, resident_mandate_ended};
pub(crate) use read::{resident_cycle_routine_disabled, resident_stall_of, resident_view};
pub(crate) use routine_status_port::ResidentRoutineStatusPort;
pub(crate) use step::{
    record_resident_continuity_rebuilt_for_run, resident_cycle_id, resident_cycle_mandate_of_run,
};
pub(crate) use task_port::TaskPort;
use view::ResidentView;
pub(crate) use view::resident_mandate_from_artifact;

#[cfg(not(feature = "test-support"))]
pub(crate) use control::expire_resident_mandates_past_deadline;
#[cfg(not(feature = "test-support"))]
pub(crate) use step::{begin_resident_cycle, finish_resident_cycle};

// Surface re-exports: what the CLI, dashboard and composition name from this subject.
pub use grant::{
    ResidentDashboardGrant, ResidentDashboardGrantOutcome, ResidentDashboardGrantParams,
};
pub use guard::take_failed_resident_action_key;
pub use mandate::ResidentEffectiveAuthority;
pub use orbit_runtime::authorization::route_resident_delivery_recovery_to_broker;

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use comment::append_resident_checkpoint;
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use control::{
    ResidentChannelCaller, ResidentControlAction, ResidentControlParams, control_resident,
    expire_resident_mandates_past_deadline,
};
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use cycle::{set_resident_live_cycle, with_resident_cycle_admission_checkpoint};
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use grant::grant_resident_authority_as_dashboard_operator;
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use guard::with_resident_action_close_failure;
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use metric::resident_metrics;
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use read::{list_resident_mandate_cards, show_resident_mandate};
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub use step::{begin_resident_cycle, finish_resident_cycle};

/// What the resident app can be asked to do.
pub enum Command {
    /// One registry verb and the fields it is called with.
    Operation(ResidentVerb, Input),
    /// Read every mandate as a card, with what the Residents view shows
    /// beside them.
    Cards {
        /// Include ended mandates.
        include_ended: bool,
    },
    /// Read one mandate as a card, for a session that is an operator's or
    /// not.
    Card {
        /// The mandate's assignment.
        task_id: String,
        /// Whether the session holds operator capability.
        operator: bool,
    },
    /// One write from an authenticated dashboard session.
    Dashboard(DashboardOrigin, DashboardWrite),
}

/// One resident write the dashboard sends.
pub enum DashboardWrite {
    /// Create a mandate's assignment and artifact.
    Create {
        /// Assignment title.
        title: String,
        /// Assignment narrative.
        description: String,
        /// Assignment complexity as the form names it; `medium` when absent.
        complexity: Option<String>,
        /// The `resident.mandate/v1` draft.
        mandate: Value,
    },
    /// Narrow, widen as an answered request, or both.
    Grant(ResidentDashboardGrantParams),
    /// The manager channel's control, as the resident control tool takes it.
    Control(Map<String, Value>),
    /// The operator's own words, as the resident relay tool takes them.
    Relay(Map<String, Value>),
}

/// The Residents view's list model.
pub struct Cards {
    /// The workspace's display label.
    pub workspace: String,
    /// One card per mandate.
    pub mandates: Vec<Value>,
    /// The configured crew registry.
    pub crews: Value,
    /// Where the workspace's resident cycle routine is defined, when the
    /// workspace has an identity.
    pub cycle_routine: Option<CycleRoutineScope>,
}

/// The workspace whose routine definitions hold its resident cycle routine.
pub struct CycleRoutineScope {
    /// The logical catalog id the dashboard addressed.
    pub workspace_id: String,
    /// The `.orbit` directory the routine is loaded from.
    pub orbit_dir: PathBuf,
}

/// What an admitted dashboard write answered.
pub enum DashboardWritten {
    /// The JSON a create, control or relay answered with.
    Value(Value),
    /// What a grant form did.
    Granted(Box<ResidentDashboardGrant>),
}

/// What a command produced.
pub enum Outcome {
    /// The JSON a registry verb's spec documents, or one card.
    Value(Value),
    /// The list model.
    Cards(Box<Cards>),
    /// A dashboard write: what it answered, or why the session was refused.
    Dashboard(DashboardWritten),
}

impl Outcome {
    /// The JSON a registry verb or card read answered with; null otherwise.
    pub fn into_value(self) -> Value {
        match self {
            Self::Value(value) => value,
            Self::Cards(_) | Self::Dashboard(_) => Value::Null,
        }
    }
}

/// Why a resident command failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A flow, or the tool entry in front of it, refused or failed the
    /// command.
    #[error(transparent)]
    Flow(#[from] OrbitError),
}

impl From<Error> for crate::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Flow(error) => error,
        }
    }
}

struct ResidentApp;

impl App for ResidentApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Command) -> Operation {
        match command {
            // The generic tool entry decides the tool row.
            Command::Operation(verb, input) => {
                Operation::of(*verb, &input.fields).without_tool_row()
            }
            Command::Cards { .. } => Operation::dashboard_read("resident", "cards"),
            Command::Card { .. } => Operation::dashboard_read("resident", "card"),
            Command::Dashboard(origin, write) => dashboard::operation(origin, write),
        }
    }

    fn run(core: &Core, command: Command) -> Result<Outcome, Error> {
        Ok(match command {
            Command::Operation(verb, input) => Outcome::Value(tool_call::run(
                core,
                verb,
                input,
                |runtime, input, caller| operation::dispatch(runtime, verb, input, caller),
            )?),
            Command::Cards { include_ended } => {
                Outcome::Cards(Box::new(dashboard::cards(core.runtime(), include_ended)?))
            }
            Command::Card { task_id, operator } => {
                Outcome::Value(dashboard::card(core, task_id, operator)?)
            }
            Command::Dashboard(_, write) => Outcome::Dashboard(dashboard::write(core, write)?),
        })
    }

    fn dashboard_row(
        result: &Result<Outcome, crate::Error>,
        _target: &str,
        arguments: Value,
    ) -> Option<DashboardRow> {
        let written = match result {
            Ok(Outcome::Dashboard(written)) => Ok(written),
            Ok(_) => return None,
            Err(error) => Err(error),
        };
        dashboard::row(written, arguments)
    }
}

/// Run one resident command: authorize it, audit it and return its outcome.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<ResidentApp>(core, command)
}
