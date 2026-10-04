mod add;
mod archive;
pub(crate) mod artifact;
pub mod artifacts;
pub(crate) mod blocked_next_step;
mod command;
mod eligible;
mod export;
pub(crate) mod flow;
mod import;
mod lint;
mod list;
pub(crate) mod output;
mod publication;
mod recheck_blocked;
mod reindex;
pub(crate) mod show;
mod update;

pub use command::{TaskCommand, TaskSubcommand};
pub use publication::TaskPublicationSubcommand;

fn mutation_identity(model: Option<String>) -> (Option<String>, Option<String>) {
    if model.is_some() {
        return (None, model);
    }
    let agent = std::env::var("ORBIT_AGENT_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let model = std::env::var("ORBIT_AGENT_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty());
    (agent, model)
}

/// Warn, without refusing, about dependency ids this machine cannot read.
///
/// A dependency may legitimately be recorded before its target exists here
/// (another workspace or host owns it), so this mirrors the `--parent`
/// warning: the value is stored either way, and a typo is still visible.
fn warn_unreadable_dependencies(runtime: &orbit_core::OrbitRuntime, dependencies: &[String]) {
    for id in dependencies {
        if runtime
            .dependency_task_is_readable(id)
            .is_ok_and(|readable| !readable)
        {
            eprintln!("warning: dependency task '{id}' was not found; recording it anyway");
        }
    }
}

#[cfg(test)]
mod tests;
