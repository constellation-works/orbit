use crate::command::{CommandOut, Execute, Payload};
use clap::Args;
use orbit_core::OrbitRuntime;
use serde_json::json;

/// Renew one selected review budget, retaining attempts and the operator decision.
#[derive(Args)]
pub struct TaskReviewResetArgs {
    /// Task belonging to the selected lineage
    pub id: String,
    /// Exact lineage key from the refusal or review manifest
    #[arg(long)]
    pub lineage: String,
    /// Explanation recorded with the operator decision
    #[arg(long)]
    pub reason: String,
    /// Adopt the current configured budget instead of the captured budget
    #[arg(long)]
    pub adopt_configured_budget: bool,
    /// Emit the updated ledger as JSON
    #[arg(long)]
    pub json: bool,
}
impl Execute for TaskReviewResetArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let value = runtime.run_tool(
            "orbit.task.review_reset",
            json!({
                "workspace": runtime.paths().repo_root,
                "id": self.id, "lineage_key": self.lineage, "reason": self.reason,
                "adopt_configured_budget": self.adopt_configured_budget,
            }),
        )?;
        Ok(Payload::detail(
            value,
            "Review budget reset; previous attempts and the decision remain in the ledger.",
        )
        .into())
    }
}
