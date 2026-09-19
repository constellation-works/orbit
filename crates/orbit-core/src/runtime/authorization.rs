//! The runtime half of the capability chokepoint.
//!
//! `orbit-common::authorization` owns the registry and the decision; this
//! module owns the two things a decision needs a runtime for — reading the
//! caller's envelope off the live tool context, and persisting the denial.
//!
//! # Why the record is written here
//!
//! Every entry surface already audits its own invocation, but not all of them
//! audit the same way and one (the dashboard's direct `run_tool`) does not
//! audit at all. Emitting the authorization row from the decision itself makes
//! "a governed operation was refused" a fact of the decision rather than a
//! property of whichever caller happened to make it, so a denial is queryable
//! with one predicate (`command = 'authorization'`) across every path.
//!
//! The entry-point row still lands too, and still carries
//! [`AuditEventStatus::Denied`]; the two rows answer different questions
//! ("what was refused" versus "what call failed").

use orbit_common::OrbitError;
use orbit_common::governance::authorization::{
    CallerCapabilities, CallerEnvelope, CallerProvenance, GovernedOperation, OperationSurface,
    authorize, governed_command, governed_tool,
};
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_store::contracts::AuditEventInsertParams;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::ToolSessionContext;

use crate::OrbitRuntime;
use crate::runtime::tool_exec::CapabilityEnforcement;

/// Canonical tool name of the trusted-host agent invocation, named once so the
/// admission and the governed-operation registry cannot drift apart.
pub(crate) const AGENT_INVOKE_OPERATION_ID: &str = "orbit.agent.invoke";

/// Identity facts retained after trusted-host admission.
#[derive(Debug)]
pub(crate) struct AgentInvokeAuthorizer {
    pub(crate) provenance: CallerProvenance,
    /// Caller label an SSH-originated MCP session forwarded, for attribution
    /// in the durable admission. Never a grant [ORB-12564].
    pub(crate) remote_caller_machine_id: Option<String>,
}

impl OrbitRuntime {
    /// Authorize a governed tool call.
    ///
    /// Called from `run_tool_with_context_and_role`, which every tool caller
    /// traverses: CLI `tool run`, the CLI's admin `run_tool` bypass, MCP
    /// `tools/call`, the dashboard, the v2 deterministic dispatcher, and agent
    /// loops. There is deliberately no second tool-side guard anywhere.
    ///
    /// An ungoverned tool has no operation-specific check, on any transport. A
    /// session that arrived over SSH is not a special case: it holds what its
    /// argv asked for, which is at least `agent` [ORB-12564].
    pub(crate) fn authorize_tool_operation(
        &self,
        tool_name: &str,
        session_context: &ToolSessionContext,
        capability_enforcement: CapabilityEnforcement,
    ) -> Result<(), OrbitError> {
        let Some(operation) = governed_tool(tool_name) else {
            return Ok(());
        };
        let capability_enforcement = if session_context.worker_invocation.is_some() {
            CapabilityEnforcement::McpSessionOnly
        } else {
            capability_enforcement
        };
        let envelope = match capability_enforcement {
            CapabilityEnforcement::Enforce => CallerEnvelope::from_process_env(session_context),
            CapabilityEnforcement::McpSessionOnly => CallerEnvelope::mcp_session(session_context),
        };
        self.decide_with_envelope(operation, envelope)
    }

    /// Admit one trusted-host agent invocation, returning how the authorizing
    /// operator was identified [ORB-11354].
    ///
    /// The single canonical admission for the unsandboxed execution mode. Every
    /// surface that can submit one — the `orbit.agent.invoke` tool and the
    /// `orbit run agent` CLI — calls this before anything durable exists, so a
    /// refusal happens before a run record and long before a process.
    ///
    /// One rule beyond the ordinary governed-operation check:
    ///
    /// * **The process envelope counts.** The tool chokepoint resolves an MCP
    ///   call session-only, which is right for placement but would let a leaf
    ///   agent shelling out to the CLI look like nothing at all. Resolving the
    ///   process envelope here means a managed run's `ORBIT_MANAGED_RUN_CONTEXT`
    ///   resolves as `agent`, and an agent is refused.
    ///
    /// A session that arrived over SSH is admitted on the same terms as a local
    /// one: holding `operator` is the whole test [ORB-12564]. There is no
    /// second, destination-side grant, because the caller reached this machine
    /// through an SSH login that already lets it start any process it likes.
    /// What keeps an *agent* from reaching here is caller-side and unchanged: a
    /// federated server running as an agent never propagates operator into a
    /// destination's argv.
    pub(crate) fn admit_agent_invoke(
        &self,
        session_context: &ToolSessionContext,
    ) -> Result<AgentInvokeAuthorizer, OrbitError> {
        let operation = governed_tool(AGENT_INVOKE_OPERATION_ID).ok_or_else(|| {
            OrbitError::Execution(format!(
                "'{AGENT_INVOKE_OPERATION_ID}' is missing from the governed operation registry"
            ))
        })?;
        let envelope = CallerEnvelope::from_process_env(session_context);
        let caller = CallerCapabilities::resolve(&envelope);
        self.decide_with_envelope(operation, envelope)?;

        Ok(AgentInvokeAuthorizer {
            provenance: caller.provenance(),
            remote_caller_machine_id: caller.remote_caller_machine_id().map(ToOwned::to_owned),
        })
    }

    /// Authorize a governed CLI command, or pass an ungoverned one through.
    ///
    /// Called once from the CLI's dispatch chokepoint. Commands reach it by
    /// name because the destructive ones (`workspace teardown`, `audit prune`)
    /// perform their destruction directly rather than through a tool, so the
    /// tool chokepoint never sees them.
    pub fn authorize_command_operation(
        &self,
        command: &str,
        subcommand: &str,
    ) -> Result<(), OrbitError> {
        let Some(operation) = governed_command(command, subcommand) else {
            return Ok(());
        };
        self.decide(operation, &ToolSessionContext::default())
    }

    fn decide(
        &self,
        operation: &'static GovernedOperation,
        session_context: &ToolSessionContext,
    ) -> Result<(), OrbitError> {
        self.decide_with_envelope(operation, CallerEnvelope::from_process_env(session_context))
    }

    fn decide_with_envelope(
        &self,
        operation: &'static GovernedOperation,
        envelope: CallerEnvelope,
    ) -> Result<(), OrbitError> {
        let caller = CallerCapabilities::resolve(&envelope);

        match authorize(operation, &caller) {
            Ok(()) => {
                if caller.is_override() {
                    // The escape hatch is allowed to be easy, not quiet.
                    tracing::warn!(
                        target: "orbit.authorization",
                        operation = operation.id,
                        provenance = %caller.provenance(),
                        "governed operation authorized through the operator override"
                    );
                    self.record_authorization_event(
                        operation.id,
                        operation.surface,
                        &caller,
                        AuditEventStatus::Success,
                        Some("authorized through the operator override".to_string()),
                    );
                }
                Ok(())
            }
            Err(denial) => {
                tracing::warn!(
                    target: "orbit.authorization",
                    operation = operation.id,
                    provenance = %denial.provenance,
                    granted = %denial.granted,
                    caller_machine_id = denial.remote_caller_machine_id.as_deref(),
                    "governed operation denied"
                );
                let message = denial.to_string();
                self.record_authorization_event(
                    operation.id,
                    operation.surface,
                    &caller,
                    AuditEventStatus::Denied,
                    Some(message.clone()),
                );
                Err(OrbitError::CapabilityDenied(message))
            }
        }
    }

    /// Persist the authorization decision.
    ///
    /// A failed write is logged and swallowed rather than converted into the
    /// caller's error. The decision itself is what the caller asked about, and
    /// on a denial the call is already being refused — turning an unwritable
    /// audit store into a *different* failure would only obscure why the
    /// operation did not run. (This is the opposite trade from
    /// `finalize_successful_dispatch`, which fails a *successful, committed*
    /// mutation whose audit row is missing; there, silence would hide a
    /// completed change.)
    fn record_authorization_event(
        &self,
        operation_id: &str,
        surface: OperationSurface,
        caller: &CallerCapabilities,
        status: AuditEventStatus,
        error_message: Option<String>,
    ) {
        // A `Tool`-surface operation is authorized from inside
        // `execute_registered_tool`, itself running inside
        // `execute_tool_dispatch_with_audit_store`'s audited closure — that
        // wrapper already writes its own row with `tool_name` set to the same
        // operation ID on the same denial. Setting it here too would make
        // `orbit audit list --tool <name>` and the denial-by-operation
        // breakdown double-count every tool-surface refusal. A
        // `CliCommand`/`Dashboard`-surface operation performs its own
        // destruction directly and never gets that second row, so it needs
        // this one to carry the operation name.
        let tool_name = match surface {
            OperationSurface::Tool => None,
            OperationSurface::CliCommand | OperationSurface::Dashboard => {
                Some(operation_id.to_string())
            }
        };
        let params = AuditEventInsertParams {
            execution_id: audit_execution_id("authz"),
            command: "authorization".to_string(),
            subcommand: Some(caller.provenance().to_string()),
            tool_name,
            target_type: Some("operation".to_string()),
            target_id: Some(operation_id.to_string()),
            // Coarse actor kind (`human` / `operator` / family), not the
            // `human:<os-user>` attribution label [ORB-12274].
            role: self.actor().audit_role().to_string(),
            status,
            exit_code: i32::from(status != AuditEventStatus::Success),
            duration_ms: 1,
            working_directory: std::env::current_dir()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|_| ".".to_string()),
            // A session that arrived over SSH names the machine it came from
            // beside the effective set. The label is attribution, not a second
            // authorization statement: it is what the caller's federated
            // server forwarded, and it never contributed to this decision
            // [ORB-12564]. `effective_capabilities` below stays the resolved
            // set every existing query reads.
            arguments_json: caller.remote_caller_machine_id().map(|caller_machine_id| {
                serde_json::json!({
                    "caller_machine_id": caller_machine_id,
                    "effective_capabilities": caller.grants(),
                })
                .to_string()
            }),
            stdout_truncated: None,
            stderr_truncated: None,
            error_message,
            host: std::env::var("HOSTNAME").ok(),
            pid: std::process::id(),
            session_id: None,
            workspace_id: None,
            caller_machine_id: caller.remote_caller_machine_id().map(ToOwned::to_owned),
            caller_host_id: None,
            process_machine_id: None,
            process_host_id: None,
            transport: None,
            effective_capabilities: caller.grants().clone(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: None,
            job_run_id: None,
            activity_id: None,
            step_index: None,
        };

        if let Err(error) = self.record_audit_event(&params) {
            tracing::error!(
                target: "orbit.authorization",
                operation = operation_id,
                "failed to persist authorization audit event: {error}"
            );
        }
    }
}
