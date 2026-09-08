use clap::Args;
use orbit_core::OrbitRuntime;

use crate::command::task::output::task_to_json_for_runtime;
use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
pub struct AutoTaskMintArgs {
    /// Definition name
    pub name: String,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for AutoTaskMintArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let task = runtime.auto_task_mint(&self.name)?;

        Ok(Payload::detail(
            task_to_json_for_runtime(runtime, &task)?,
            format!("{} {}", task.id, task.title),
        )
        .into())
    }
}
