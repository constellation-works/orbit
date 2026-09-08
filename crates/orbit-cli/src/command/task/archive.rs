use clap::Args;
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, Execute, Payload};

use super::output::task_to_json_for_runtime;

#[derive(Args)]
#[command(after_help = "Restore an archived task by updating it to any other status.")]
pub struct TaskArchiveArgs {
    /// Task ID
    pub id: String,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for TaskArchiveArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        runtime.archive_task(&self.id)?;
        let task = runtime.get_task(&self.id)?;
        Ok(Payload::detail(
            task_to_json_for_runtime(runtime, &task)?,
            format!("Archived task '{}'", self.id),
        )
        .into())
    }
}
