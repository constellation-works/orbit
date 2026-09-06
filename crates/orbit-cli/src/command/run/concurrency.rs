//! `orbit run concurrency` — retune a live drain's worker ceiling [ORB-11253].

use clap::Args;
use orbit_core::{DrainWorkerLimitRequest, OrbitRuntime};
use serde_json::json;

use crate::command::{CommandOut, CommandOutput, Execute, Payload};

#[derive(Args)]
#[command(
    about = "Change how many tasks a running drain keeps in flight",
    after_help = "Adjusts the worker ceiling of a live `orbit run auto` window in place. The \
run ID, its deadline, its completion authorization, and every child it has already dispatched \
are preserved — this is the supported alternative to cancelling the drain and submitting a \
replacement.\n\nRaising the ceiling takes effect on the next admission pass. Lowering it stops \
new admissions until enough children finish; it never cancels or shortens a task already in \
flight.\n\n`--if-revision` makes the change conditional: pass the revision `orbit run show` \
reported and a concurrent adjustment is refused rather than overwritten.\n\nExamples:\n  \
orbit run concurrency jrun-20260905-0546 --set 7\n  orbit run concurrency jrun-20260905-0546 \
--set 3 --reason 'provider rate limited'\n  orbit run concurrency jrun-20260905-0546 --set 7 \
--if-revision 2 --json"
)]
pub struct RunConcurrencyArgs {
    /// Job run ID of the drain to retune
    pub run_id: String,

    /// New number of tasks the drain may keep in flight
    #[arg(long = "set", value_name = "N")]
    pub set: u32,

    /// Note recorded with the change
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,

    /// Apply only while the ceiling is still at this revision
    #[arg(long = "if-revision", value_name = "N")]
    pub if_revision: Option<u32>,

    /// Token for this workspace's exclusive claim, when another operator holds
    /// one. Falls back to `ORBIT_WORKSPACE_CLAIM_TOKEN`.
    #[arg(long)]
    pub claim_token: Option<String>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for RunConcurrencyArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let change = runtime.set_drain_worker_limit(DrainWorkerLimitRequest {
            run_id: &self.run_id,
            max_active_leaf_runs: self.set,
            expected_revision: self.if_revision,
            reason: self.reason.as_deref(),
            actor: "cli",
            source: "run_concurrency",
            claim_token: self.claim_token.as_deref(),
        })?;
        if self.json {
            return Ok(Payload::document(json!({
                "run_id": change.run_id,
                "job_id": change.job_id,
                "outcome": change.outcome,
                "previous_concurrency": change.previous_max_active_leaf_runs,
                "concurrency": change.max_active_leaf_runs,
                "revision": change.revision,
                "hard_limit": change.hard_limit,
            }))
            .into());
        }
        if change.outcome == "unchanged" {
            println!(
                "job run {} already admits {} tasks at a time (revision {})",
                change.run_id, change.max_active_leaf_runs, change.revision
            );
        } else {
            println!(
                "job run {} now admits {} tasks at a time, was {} (revision {})",
                change.run_id,
                change.max_active_leaf_runs,
                change.previous_max_active_leaf_runs,
                change.revision
            );
            println!(
                "Running children are untouched; the new ceiling applies to the next admission pass."
            );
        }
        Ok(CommandOutput::Silent)
    }
}
