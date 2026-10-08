use crate::command::{CommandOut, Execute, Payload};
use clap::Args;
use orbit_core::OrbitRuntime;
use serde_json::{Value, json};

/// Renew one selected review budget, retaining attempts and the operator decision.
#[derive(Args)]
pub struct TaskReviewResetArgs {
    #[command(flatten)]
    pub(crate) routing: super::command::TaskHostArgs,
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
}
/// What a reset prints beside its ledger, here or routed to another host.
pub(crate) const REVIEW_RESET_TEXT: &str =
    "Review budget reset; previous attempts and the decision remain in the ledger.";

impl TaskReviewResetArgs {
    /// The `orbit.task.review_reset` input, without a workspace selector.
    pub(crate) fn tool_input(&self) -> Value {
        json!({
            "id": self.id, "lineage_key": self.lineage, "reason": self.reason,
            "adopt_configured_budget": self.adopt_configured_budget,
        })
    }
}

impl Execute for TaskReviewResetArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let mut input = self.tool_input();
        input["workspace"] = json!(runtime.paths().repo_root);
        let value = runtime.run_tool("orbit.task.review_reset", input)?;
        Ok(Payload::detail(value, REVIEW_RESET_TEXT).into())
    }
}
