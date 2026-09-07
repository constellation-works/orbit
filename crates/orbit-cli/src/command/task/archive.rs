use clap::Args;
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, CommandOutput, Execute, Payload};

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
        if self.json {
            let task = runtime.get_task(&self.id)?;
            Ok(Payload::document(task_to_json_for_runtime(runtime, &task)?).into())
        } else {
            println!("Archived task '{}'", self.id);
            Ok(CommandOutput::Silent)
        }
    }
}
