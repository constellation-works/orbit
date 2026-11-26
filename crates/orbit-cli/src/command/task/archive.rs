use clap::Args;
use orbit_core::Core;
use orbit_core::app::task::{self, Archive, Command, Outcome};

use crate::command::{CommandOut, ExecuteCore, Payload};

use super::output::warn_unresolved_crew;

#[derive(Args)]
#[command(
    after_help = "Archived is terminal: restore a task with `task update <id> --status <status> --force`."
)]
pub struct TaskArchiveArgs {
    /// Task ID
    pub id: String,
    /// Archive even if a live local run owns the task
    #[arg(long)]
    pub force: bool,
}

impl ExecuteCore for TaskArchiveArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let document = task::exec(
            core,
            Command::Archive(Archive {
                id: self.id.clone(),
                force: self.force,
            }),
        )
        .and_then(Outcome::into_answer)?;
        if let Some(doc) = document.as_object() {
            warn_unresolved_crew(doc);
        }
        Ok(Payload::detail(document, format!("Archived task '{}'", self.id)).into())
    }
}
