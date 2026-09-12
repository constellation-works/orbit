use clap::Args;
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::workflow::automation::recovery::{RecoveryPreview, RecoveryRequest};

use crate::command::{CommandOut, Execute, Payload};

/// Recover a delivery consumer that stalled on a settings change.
///
/// Without `--adopt-settings` or `--reissue-action` this is a read-only
/// preview. Neither operation covers, waives or discards an obligation, and
/// state files are never edited by hand.
#[derive(Args)]
pub struct AutoTaskRecoverArgs {
    /// Definition name
    pub name: String,
    /// Adopt the definition's current settings, retaining every pending,
    /// unresolved and accepted fact. Refused when the repository, branch,
    /// owner or coverage class changed, or while an action is executing.
    #[arg(long)]
    pub adopt_settings: bool,
    /// Reissue the settled action that closed without accepted evidence, as a
    /// new task over its existing frozen obligations. The closed task is left
    /// exactly as it settled.
    #[arg(long)]
    pub reissue_action: bool,
    /// Operator explanation, retained in the recovery audit record. Required
    /// for either operation.
    #[arg(long)]
    pub reason: Option<String>,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for AutoTaskRecoverArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let definition = runtime.auto_task_show(&self.name)?.ok_or_else(|| {
            OrbitError::InvalidInput(format!("no such auto-task '{}'", self.name))
        })?;

        let request = RecoveryRequest {
            adopt_settings: self.adopt_settings,
            reissue_action: self.reissue_action,
            reason: self.reason.unwrap_or_default(),
        };

        let preview = orbit_core::application::automation::recover_auto_task(
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

fn summary(name: &str, preview: &RecoveryPreview) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(out, "{name} ({})", preview.reason);
    let _ = writeln!(out, "  consumer: {}", preview.consumer);
    let _ = writeln!(
        out,
        "  identity: {} -> {}",
        preview.identity.recorded_epoch, preview.identity.configured_epoch
    );
    if !preview.identity.changes.is_empty() {
        let _ = writeln!(out, "  changed: {}", preview.identity.changes.join(", "));
    }

    let debt = &preview.debt;
    let _ = writeln!(out, "  covered: {}", debt.covered.commit);
    let _ = writeln!(
        out,
        "  debt: {} pending deliveries, {} pending commits, {} unresolved, {} waived, {} excluded, {} receipts",
        debt.pending_deliveries,
        debt.pending_commits,
        debt.unresolved,
        debt.waived,
        debt.excluded,
        debt.receipts
    );

    if let Some(action) = &preview.action {
        let _ = writeln!(
            out,
            "  action: {} attempt {} ({:?}), {} obligations, {} commits, reissuable={}",
            action.batch_id,
            action.attempt,
            action.state,
            action.obligations.len(),
            action.commits,
            action.reissuable
        );
    }

    if preview.applied.is_empty() {
        let _ = writeln!(out, "  applied: none (preview)");
    } else {
        let _ = writeln!(out, "  applied: {}", preview.applied.join(", "));
    }

    if !preview.refusals.is_empty() {
        let _ = writeln!(out, "  refusals: {}", preview.refusals.join(", "));
    }

    out
}
