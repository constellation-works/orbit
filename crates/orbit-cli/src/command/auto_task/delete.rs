use clap::Args;
use orbit_core::{AutoTaskDeleteParams, AutoTaskDeleteReport, OrbitError, OrbitRuntime};

use crate::command::{CommandOut, Execute, Payload};

/// Delete a definition with its scheduler cursor and delivery consumer state.
///
/// Refuses while a task minted from the definition is still open. Deleting a
/// shipped default records an opt-out, so `orbit workspace init --force` and
/// `orbit workspace sync` do not re-create it; `orbit auto-task restore`
/// brings it back.
#[derive(Args)]
pub struct AutoTaskDeleteArgs {
    /// Definition name
    pub name: String,
    /// Why the definition is deleted, kept in the audit record
    #[arg(long)]
    pub reason: Option<String>,
    /// Delete even while a minted task is open or a delivery action is
    /// executing. The open tasks stay as they are.
    #[arg(long)]
    pub force: bool,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for AutoTaskDeleteArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let report = runtime.auto_task_delete(AutoTaskDeleteParams {
            name: self.name,
            reason: self.reason,
            force: self.force,
        })?;
        let document = serde_json::to_value(&report)
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        Ok(Payload::detail(document, summary(&report)).into())
    }
}

fn summary(report: &AutoTaskDeleteReport) -> String {
    let mut out = format!("{} deleted", report.name);
    if report.opted_out {
        out.push_str(
            "; shipped default opted out of reseeding (`orbit auto-task restore` reinstates it)",
        );
    }
    if let Some(consumer) = &report.consumer
        && (consumer.reset || !consumer.released_refs.is_empty())
    {
        out.push_str(&format!(
            "; delivery consumer reset, {} pinned refs released",
            consumer.released_refs.len()
        ));
    }
    if !report.open_tasks.is_empty() {
        out.push_str(&format!("; left open: {}", report.open_tasks.join(", ")));
    }
    out
}
