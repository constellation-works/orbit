use clap::Args;
use orbit_core::OrbitRuntime;

use serde_json::json;

use crate::command::{CommandOut, Execute, Payload, require_confirmation};
use crate::parse::parse_since;

#[derive(Args)]
pub struct AuditPruneArgs {
    /// Prune events older than this duration (e.g. "90d", "1h")
    #[arg(long)]
    pub older_than: String,
    /// Confirm permanent deletion of matching audit rows
    #[arg(long)]
    pub confirm: bool,
}

impl Execute for AuditPruneArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        // Validate the duration first so a typo is reported as a typo rather
        // than hidden behind the confirmation prompt.
        let cutoff = parse_since(&self.older_than)?;
        require_confirmation(self.confirm, "audit pruning")?;
        let pruned = runtime.prune_audit_events(&cutoff)?;
        Ok(Payload::detail(
            json!({ "pruned": pruned }),
            format!("Pruned {pruned} audit events"),
        )
        .into())
    }
}
