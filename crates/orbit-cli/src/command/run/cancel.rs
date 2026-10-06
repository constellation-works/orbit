//! `orbit run cancel` — terminalize a pending/running job run [ORB-10070].

use clap::Args;
use orbit_core::OrbitRuntime;
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload, require_confirmation};

#[derive(Args)]
#[command(
    about = "Cancel a job run, or report its existing terminal outcome",
    after_help = "Cancels a job run that has not reached a terminal state: signals the \
owner process of a running run (TERM then KILL), stops the agent processes \
the run still has open, releases the run's task reservations, and finalizes \
the run as `cancelled`. The primary remediation \
for a stuck `pending` run with no live worker (orphan reconciliation also \
clears those on workspace open). A run that already finished returns a stable \
`already_terminal` result without replacing its outcome.\n\n\
A running follower pull drain (`workspace_pull_pipeline`) is cancelled \
gracefully: it stops requesting work at once, returns the claims it had not \
launched to the owner's backlog, and keeps running — reported as \
`cancelling`, with the leaves it is waiting for — until every launched leaf \
has finished and its outcome reached the owner; then it ends `cancelled`. \
The command returns immediately; `orbit run show <run_id>` follows the wait. \
`--force` does not wait: it stops the drain and each of its running leaves, \
and returns every claim to the owner's backlog with a comment naming the \
drain and the reason. It touches only the leaves that drain carries. A leaf \
whose stop cannot be confirmed keeps its claim on the owner, is listed, and \
makes the command exit 1. A queued drain, or one whose worker is gone, is \
cancelled at once. Cancelling a drain that already ended delivers whatever it \
left behind, and `--force` also stops leaves it left running.\n\n\
`--force` on a local auto drain (`workspace_auto_pipeline`) also cancels the \
task runs it started, which a plain cancel leaves running. A child whose stop \
cannot be confirmed is listed with its reason and makes the command exit 1. For any other run \
it changes nothing.\n\n\
Cancelling a task leaf returns its task to the backlog with the reason and \
keeps the candidate available to resume. `--block` keeps the task blocked for \
the existing manual recovery flow.\n\nExamples:\n  orbit run cancel jrun-20260706-0120-2 --confirm\n  orbit run cancel jrun-20260706-0120-2 --confirm --block --reason \"needs review\"\n  orbit run cancel jrun-20260706-0120-2 --confirm --force --reason \"host maintenance\"\n  orbit run cancel jrun-20260706-0120-2 --confirm --json"
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

    /// Optional reason recorded with the cancellation audit event
    #[arg(long)]
    pub reason: Option<String>,

    /// Do not wait for a drain's in-flight leaves: stop them too, returning a
    /// pull drain's claims to the owner's backlog
    #[arg(long)]
    pub force: bool,

    /// Keep a cancelled task leaf blocked instead of returning it to the backlog
    #[arg(long)]
    pub block: bool,
}

impl Execute for RunCancelArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        require_confirmation(self.confirm, "run cancellation")?;
        let result = runtime.cancel_job_run_with_options_and_policy(
            &self.run_id,
            "cli",
            "run_cancel",
            self.reason.as_deref(),
            self.force,
            self.block,
        )?;
        let doc = json!({
            "run_id": result.run_id,
            "outcome": result.outcome,
            "previous_state": result.previous_state,
            "final_state": result.final_state,
            "signal_attempted": result.signal_attempted,
            "signal_outcome": result.signal_outcome,
            "provider_processes_stopped": result.provider_processes_stopped,
            "pull_settlements": super::support::pull_settlements_json(&result.pull_settlements),
            "waiting_leaves": result.waiting_leaves,
            "forced_runs": result.forced_runs,
            "unstopped_leaves": result.unstopped_leaves,
            "unstopped_children": result.unstopped_children,
        });
        let mut lines = Vec::new();
        if result.outcome == "cancelling" {
            lines.push(format!(
                "cancelling job run {}: waiting for {} leaves; it ends `cancelled` once they \
                 finish and settle (`--force` stops them instead)",
                result.run_id,
                result.waiting_leaves.len()
            ));
            lines.extend(
                result
                    .waiting_leaves
                    .iter()
                    .map(|leaf| format!("  {}", leaf.describe())),
            );
        } else if result.outcome == "already_terminal" {
            lines.push(format!(
                "job run {} was already terminal ({})",
                result.run_id, result.final_state
            ));
        } else {
            lines.push(format!(
                "cancelled job run {} ({} -> {})",
                result.run_id, result.previous_state, result.final_state
            ));
        }
        if let Some(outcome) = &result.signal_outcome {
            lines.push(format!("owner process signal outcome: {outcome}"));
        }
        if !result.forced_runs.is_empty() {
            lines.push(format!(
                "stopped with --force: {}",
                result.forced_runs.join(", ")
            ));
        }
        if result.provider_processes_stopped > 0 {
            lines.push(format!(
                "provider processes stopped: {}",
                result.provider_processes_stopped
            ));
        }
        lines.extend(super::support::pull_settlement_lines(
            &result.pull_settlements,
        ));
        if result.unstopped_leaves.is_empty() && result.unstopped_children.is_empty() {
            return Ok(Payload::detail(doc, lines.join("\n")).into());
        }
        // The parent is cancelled, but any unconfirmed leaf or detached
        // child makes the forced request incomplete.
        if !result.unstopped_leaves.is_empty() {
            lines.push(format!(
                "not stopped with --force ({} leaves; their claims stay with the owner):",
                result.unstopped_leaves.len()
            ));
            lines.extend(
                result
                    .unstopped_leaves
                    .iter()
                    .map(|leaf| format!("  {}", leaf.describe())),
            );
        }
        if !result.unstopped_children.is_empty() {
            lines.push(format!(
                "not stopped with --force ({} detached children):",
                result.unstopped_children.len()
            ));
            lines.extend(
                result
                    .unstopped_children
                    .iter()
                    .map(|child| format!("  {}", child.describe())),
            );
        }
        Ok(Payload::detail(doc, lines.join("\n"))
            .with_exit_code(1)
            .into())
    }
}
