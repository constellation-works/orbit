use clap::Args;
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::workflow::automation::recovery::{ResetPreview, ResetRequest};

use crate::command::{CommandOut, Execute, Payload};

/// Forget one delivery consumer's coverage debt and re-baseline it.
///
/// Without `--reason` this only previews what would be forgotten. Reset is the
/// escape hatch for a consumer no recovery can repair — it discards every
/// pending, unresolved, waived and excluded fact rather than retaining them, so
/// the next evaluation starts from the current branch head.
#[derive(Args)]
pub struct AutoTaskResetArgs {
    /// Definition name
    pub name: String,
    /// Operator explanation, retained in the audit record. Required to apply;
    /// without it the command previews and writes nothing.
    #[arg(long)]
    pub reason: Option<String>,
    /// Reset even while an action is executing. The admitted task or run is
    /// abandoned, not cancelled.
    #[arg(long)]
    pub force: bool,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for AutoTaskResetArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let definition = runtime.auto_task_show(&self.name)?.ok_or_else(|| {
            OrbitError::InvalidInput(format!("no such auto-task '{}'", self.name))
        })?;

        let request = ResetRequest {
            reason: self.reason.unwrap_or_default(),
            force: self.force,
        };

        let preview = orbit_core::application::automation::reset_auto_task(
            runtime,
            &definition,
            &request,
            chrono::Utc::now(),
        )?;

        let document = serde_json::to_value(&preview)
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;

        Ok(Payload::detail(document, summary(&self.name, &preview)).into())
    }
}

fn summary(name: &str, preview: &ResetPreview) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(out, "{name} ({})", preview.reason);
    let _ = writeln!(out, "  consumer: {}", preview.consumer);
    let _ = writeln!(
        out,
        "  generation: {} (epoch {})",
        preview.generation, preview.epoch
    );

    let debt = &preview.debt;
    let _ = writeln!(
        out,
        "  forgets: {} pending deliveries, {} pending commits, {} unresolved, {} waived, {} excluded, {} receipts",
        debt.pending_deliveries,
        debt.pending_commits,
        debt.unresolved,
        debt.waived,
        debt.excluded,
        debt.receipts
    );
    let _ = writeln!(
        out,
        "  covered: {} observed: {}",
        debt.covered.commit, debt.observed.commit
    );

    if let Some(action) = &preview.action {
        let _ = writeln!(
            out,
            "  action: {} attempt {} ({:?}), {} obligations, {} commits",
            action.batch_id,
            action.attempt,
            action.state,
            action.obligations.len(),
            action.commits
        );
    }

    if let Some(stall) = &preview.stall {
        let _ = writeln!(
            out,
            "  stalled: {} since {}",
            stall.reason,
            stall.since.to_rfc3339()
        );
    }

    let _ = writeln!(out, "  re-baselines at: {}", preview.baseline.commit);

    if preview.applied {
        let _ = writeln!(out, "  applied: reset");
    } else {
        let _ = writeln!(out, "  applied: none (preview; pass --reason to apply)");
    }

    if !preview.refusals.is_empty() {
        let _ = writeln!(out, "  refusals: {}", preview.refusals.join(", "));
    }

    out
}
