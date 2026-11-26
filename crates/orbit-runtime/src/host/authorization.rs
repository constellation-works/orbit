//! Tool-surface authorization and activity-policy denial recording on [`RuntimeKernel`].

use orbit_common::OrbitError;
use orbit_common::governance::authorization::{
    CallerCapabilities, CallerEnvelope, OperationSurface, governed_plugin_tool, governed_tool,
};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::ToolSessionContext;

use super::exec::tool_exec::CapabilityEnforcement;
use crate::RuntimeKernel;
use crate::base::policy::{DECISION_DIGEST_OPERATION_ID, DECISION_SUBMIT_OPERATION_ID};

impl RuntimeKernel {
    /// Authorize a governed tool call.
    ///
    /// Called from `run_tool_with_context`, which every tool caller
    /// traverses: CLI `tool run`, the CLI's admin `run_tool` bypass, MCP
    /// `tools/call`, the dashboard, the v2 deterministic dispatcher, and agent
    /// loops. There is deliberately no second tool-side guard anywhere.
    ///
    /// An ungoverned tool has no operation-specific check, on any transport. A
    /// session that arrived over SSH is not a special case: it holds what its
    /// argv asked for, which is at least `agent` [ORB-12564].
    pub fn authorize_tool_operation(
        &self,
        tool_name: &str,
        session_context: &ToolSessionContext,
        capability_enforcement: CapabilityEnforcement,
    ) -> Result<(), OrbitError> {
        // A plugin tool is authorized by the generic row for its manifest's
        // execution kind; every other tool by its own row, if it has one.
        let operation = match self.tool_registry().plugin_binding(tool_name) {
            Some(binding) => governed_plugin_tool(
                binding.execution_kind == orbit_types::plugin::PluginExecutionKind::Mutating,
                binding.agent_callable,
            ),
            None => match governed_tool(tool_name) {
                Some(operation) => operation,
                None => return Ok(()),
            },
        };
        // The broker executes an authenticated resident's right-gated operations
        // directly on the tool host. Every caller here must satisfy the row.
        // The decision channel's submission and digest (design
        // `docs/design/human-decision-channel/2_design.md` §3.3, §7):
        // their governed rows are operator-only, but the caller the rows
        // exist for — a registered channel plugin's backend — holds no
        // capability, only a host-issued callback session. An *identified*
        // callback passes this row and is decided by the checks that can
        // actually judge it: the callback allowlist at the tool entry (the
        // plugin must be granted this tool, under its session ceiling) and
        // the handler's registration binding (and, for submission, token
        // verification). Every other caller, agent sessions included, is
        // held to the row.
        if (tool_name == DECISION_SUBMIT_OPERATION_ID || tool_name == DECISION_DIGEST_OPERATION_ID)
            && session_context.worker_invocation.is_none()
            && matches!(
                orbit_tools::plugin::resolve_plugin_callback_session(
                    self.env(),
                    &self.global_root()
                )?,
                orbit_tools::plugin::CallbackResolution::Identified(_)
            )
        {
            return Ok(());
        }
        let capability_enforcement = if session_context.worker_invocation.is_some() {
            CapabilityEnforcement::McpSessionOnly
        } else {
            capability_enforcement
        };
        let envelope = match capability_enforcement {
            CapabilityEnforcement::Enforce => {
                CallerEnvelope::from_process_env(self.env(), session_context)
            }
            CapabilityEnforcement::McpSessionOnly => CallerEnvelope::mcp_session(session_context),
        };
        self.decide_with_envelope(operation, envelope)
    }

    /// Keep activity-policy refusals queryable through the same authorization
    /// predicate as capability refusals, including tools with no governed row.
    pub(crate) fn record_activity_tool_policy_denial(
        &self,
        name: &str,
        reason: &str,
        session: &ToolSessionContext,
        enforcement: CapabilityEnforcement,
    ) {
        let envelope = match enforcement {
            CapabilityEnforcement::Enforce if session.worker_invocation.is_none() => {
                CallerEnvelope::from_process_env(self.env(), session)
            }
            _ => CallerEnvelope::mcp_session(session),
        };
        self.record_authorization_event(
            name,
            OperationSurface::Tool,
            &CallerCapabilities::resolve(&envelope),
            AuditEventStatus::Denied,
            Some(reason.to_string()),
            session.trace_id.as_deref(),
        );
    }
}
