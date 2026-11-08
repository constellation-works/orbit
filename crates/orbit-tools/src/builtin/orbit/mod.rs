mod agents;
mod auto_task;
mod automation;
mod codebase;
pub mod command;
mod decision;
mod domain_control;
pub mod drain;
mod evolution;
mod experiment;
mod memory;
mod notice;
pub mod operation;
mod orchestrator;
pub mod pipeline;
mod resident;
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

mod attention;
