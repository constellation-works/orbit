//! `orbit run cancel` — terminalize a pending/running job run [ORB-10070].

use clap::Args;
use orbit_core::OrbitRuntime;
use serde_json::json;

use crate::command::{CommandOut, CommandOutput, Execute, Payload, require_confirmation};

#[derive(Args)]
#[command(
    about = "Cancel a job run, or report its existing terminal outcome",
    after_help = "Cancels a job run that has not reached a terminal state: signals the \
owner process of a running run (TERM then KILL), releases the run's task \
reservations, and finalizes the run as `cancelled`. The primary remediation \
for a stuck `pending` run with no live worker (orphan reconciliation also \
clears those on workspace open). A run that already finished returns a stable \
`already_terminal` result without replacing its outcome.\n\nExamples:\n  orbit run cancel jrun-20260706-0120-2 --confirm\n  orbit run cancel jrun-20260706-0120-2 --confirm --json"
)]
pub struct RunCancelArgs {
    /// Job run ID to cancel
    pub run_id: String,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Confirm process termination and irreversible run terminalization
    #[arg(long)]
    pub confirm: bool,
}

impl Execute for RunCancelArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        require_confirmation(self.confirm, "run cancellation")?;
        let result = runtime.cancel_job_run_with_context(&self.run_id, "cli", "run_cancel")?;
        if self.json {
            return Ok(Payload::document(json!({
                "run_id": result.run_id,
                "outcome": result.outcome,
                "previous_state": result.previous_state,
                "final_state": result.final_state,
                "signal_attempted": result.signal_attempted,
                "signal_outcome": result.signal_outcome,
            }))
            .into());
        }
        if result.outcome == "already_terminal" {
            println!(
                "job run {} was already terminal ({})",
                result.run_id, result.final_state
            );
        } else {
            println!(
                "cancelled job run {} ({} -> {})",
                result.run_id, result.previous_state, result.final_state
            );
        }
        if let Some(outcome) = &result.signal_outcome {
            println!("owner process signal outcome: {outcome}");
        }
        Ok(CommandOutput::Silent)
    }
}
