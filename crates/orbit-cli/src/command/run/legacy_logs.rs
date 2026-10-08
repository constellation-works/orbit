use clap::Args;
use orbit_core::OrbitRuntime;

use crate::command::run;
use crate::command::{CommandOut, Execute};

#[derive(Args)]
#[command(about = "View execution logs for a job run")]
pub struct LogsCommand {
    /// Run ID to inspect
    pub run_id: String,

    /// Show only a specific step by target_id
    #[arg(long)]
    pub step: Option<String>,
}

impl Execute for LogsCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        eprintln!(
            "[deprecated] use \"orbit run show {}\" for the step table or \"orbit run logs {}\" for raw stdout/stderr",
            self.run_id, self.run_id
        );
        run::legacy_logs_summary_payload(runtime, &self.run_id, self.step.as_deref())
    }
}
