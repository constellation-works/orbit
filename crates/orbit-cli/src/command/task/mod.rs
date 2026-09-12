mod add;
mod archive;
pub(crate) mod artifact;
pub mod artifacts;
mod command;
mod export;
pub(crate) mod flow;
mod import;
mod lint;
mod list;
pub(crate) mod output;
mod publication;
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

#[cfg(test)]
mod tests;
