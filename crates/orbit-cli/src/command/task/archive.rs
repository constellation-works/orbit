use clap::Args;
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, Execute, Payload};

use super::output::task_to_json_for_runtime;

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

impl Execute for TaskArchiveArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        orbit_core::application::task::with_task_write_provenance(
            orbit_types::task::TaskWriteProvenance::new(
                Some(orbit_types::task::TaskWriteSurface::Cli),
                None,
            ),
            || runtime.archive_task_as_operator(&self.id, self.force),
        )?;
        let task = runtime.get_task(&self.id)?;
        Ok(Payload::detail(
            task_to_json_for_runtime(runtime, &task)?,
            format!("Archived task '{}'", self.id),
        )
        .into())
    }
}
