use std::ffi::OsStr;

use orbit_common::Env;
mod archive;
pub(crate) mod artifact;
mod command;
mod eligible;
mod export;
pub(crate) mod flow;
mod import;
mod lint;
mod list;
pub(crate) mod operation;
pub(crate) mod output;
mod pilot_comment;
mod publication;
mod recheck_blocked;
pub(crate) mod reconcile_review;
mod reindex;
pub(crate) mod review_reset;
mod settle_merged;
mod show;
pub(crate) mod support;

pub(crate) use show::{
    completion_hold_text, final_recovery_text, next_step_text, owed_criteria_lines,
};

pub(crate) use command::TaskInvocation;
pub use command::{TaskCommand, TaskSubcommand};
pub use publication::TaskPublicationSubcommand;

pub(crate) fn mutation_identity(
    model: Option<String>,
    env: &Env,
) -> (Option<String>, Option<String>) {
    if model.is_some() {
        return (None, model);
    }
    let agent = env
        .orbit_agent_name()
        .and_then(OsStr::to_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned);
    let model = env
        .orbit_agent_model()
        .and_then(OsStr::to_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned);
    (agent, model)
}

#[cfg(test)]
mod tests;
