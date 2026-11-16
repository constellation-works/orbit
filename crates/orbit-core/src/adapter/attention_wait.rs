//! The shell attention wait: a session-minted grant bounds what a caller with
//! no capability may read, and each poll runs as an agent-capability session.
use crate::OrbitRuntime;
use crate::application::attention::{
    AttentionWaitOutcome, AttentionWaitScope, wait_for_attention_delta,
};
use orbit_common::OrbitError;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::Value;

impl OrbitRuntime {
    /// Wait for an attention delta on behalf of a shell that presents a wait
    /// grant minted by a session.
    ///
    /// Every poll re-redeems the grant, so an expiry mid-wait ends it, and runs
    /// as an agent-capability session limited to the grant's workspaces:
    /// runtime kinds and host health stay `degraded` as for any agent, and the
    /// whole-host, cross-host and grant-minting inputs are refused. A token
    /// that is missing, unknown or expired is `capability_denied`, never read
    /// as clean.
    pub fn attention_wait(
        &self,
        token: &str,
        input: Value,
        timeout: &str,
        interval: Option<&str>,
    ) -> Result<AttentionWaitOutcome, OrbitError> {
        wait_for_attention_delta(input, timeout, interval, &mut |input| {
            let scope = AttentionWaitScope::redeem(&self.global_root(), token)?;
            scope.check_input(&input)?;
            let session = ToolSessionContext {
                effective_capabilities: [McpCapability::Agent].into(),
                ..ToolSessionContext::default()
            };
            let response = self.run_tool_with_session("orbit.attention", input, session)?;
            scope.check_response(&response)?;
            Ok(response)
        })
    }
}
