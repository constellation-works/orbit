//! Task and job actions of auto-tasks and routines, and the state scheduler's
//! side of the task-owned automation port.

mod actions;
mod port;

pub(crate) use actions::accepted_action_coverage;
pub(super) use actions::{action_id, job_outcome, mint, outcome};
