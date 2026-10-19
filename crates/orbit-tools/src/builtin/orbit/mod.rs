pub mod agent;
mod auto_task;
mod codebase;
pub mod command;
mod domain_control;
pub mod drain;
pub mod operation;
mod orchestrator;
pub mod pipeline;
pub mod search;
pub mod task;
mod workflow;
pub mod workspace_claim;

mod dispatch;
mod identity;
mod register;
mod workspace;

pub use self::register::register;

use self::dispatch::{
    execute_host_action, reject_agent_field, reject_unknown_tool_arguments, report_unknown_fields,
};
use self::identity::{identity_params, model_identity_params};
use self::workspace::{apply_session_orchestrator_default, resolve_workspace_argument};
