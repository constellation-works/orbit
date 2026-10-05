pub mod agent;
pub mod auto_task;
pub mod command;
mod domain_control;
pub mod drain;
pub mod friction;
pub mod operation;
pub mod pipeline;
pub mod search;
pub mod task;
pub mod workflow;
pub mod workspace_claim;

mod dispatch;
mod identity;
mod register;
mod workspace;

pub use self::register::register;

pub(super) use orbit_common::protocol::tool_input::{optional_string_alias, required_string};

use self::dispatch::{
    execute_host_action, orbit_id_params, reject_agent_field, reject_unknown_tool_arguments,
};
use self::identity::{identity_params, model_identity_params};
use self::workspace::{apply_session_orchestrator_default, resolve_workspace_argument};

#[cfg(test)]
mod tests;
