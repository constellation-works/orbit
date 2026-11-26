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
    CallerCapabilities, CallerEnvelope, GovernedOperation, OperationSurface,
    TASK_GRANT_TOOLS_OPERATION_ID, authorize, governed_command, governed_tool,
};
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_store::contract::{AuditEventInsertParams, AuditInvocationFields};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::ToolSessionContext;
use orbit_types::workflow::RESIDENT_CYCLE_JOB;

use crate::RuntimeKernel;
use crate::base::policy::{OperatorAuthorizer, TASK_REQUEUE_OPERATION_ID};

impl RuntimeKernel {
    /// Routing hint for the nested client. Only the broker authenticates the run.
    pub fn env_names_resident_cycle(&self, env: &orbit_common::Env) -> bool {
        crate::base::managed_env::managed_run_context_run_id_from_env(env)
            .is_some_and(|run_id| self.is_resident_cycle_run(&run_id))
    }

    /// Whether `run_id` is a resident cycle's run in this workspace. A failed
    /// lookup is not one.
    pub fn is_resident_cycle_run(&self, run_id: &str) -> bool {
        matches!(
            self.stores().jobs().get_job_run(run_id),
            Ok(Some(run)) if run.job_id == RESIDENT_CYCLE_JOB
        )
    }

    /// Admit one governed operator operation against the process envelope as
    /// well as the session, so a managed run shelling out to the CLI resolves
    /// as `agent` and is refused. Returns how the operator was identified.
    pub fn admit_operator_operation(
        &self,
        operation_id: &str,
        session_context: &ToolSessionContext,
    ) -> Result<OperatorAuthorizer, OrbitError> {
        let operation = governed_tool(operation_id).ok_or_else(|| {
            OrbitError::Execution(format!(
                "'{operation_id}' is missing from the governed operation registry"
            ))
        })?;
        let envelope = CallerEnvelope::from_process_env(self.env(), session_context);
        let caller = CallerCapabilities::resolve(&envelope);
        self.decide_with_envelope(operation, envelope)?;

        Ok(OperatorAuthorizer {
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
        self.decide_with_envelope(
            operation,
            CallerEnvelope::from_process_env(self.env(), session_context),
        )
    }

    pub(crate) fn decide_with_envelope(
        &self,
        operation: &'static GovernedOperation,
        envelope: CallerEnvelope,
    ) -> Result<(), OrbitError> {
        let caller = CallerCapabilities::resolve_for_operation(&envelope, operation);

        match authorize(operation, &caller) {
            Ok(()) => {
                if caller.is_override() {
                    // The CLI already owns the terminal surface and the audit
                    // row records the override, so stay off stderr at the
                    // default level; the record remains at debug level.
                    tracing::debug!(
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
                        None,
                    );
                } else if operation.id == TASK_GRANT_TOOLS_OPERATION_ID {
                    // Grant writes need an authorization trail for ordinary
                    // operator sessions too, not only explicit CLI overrides.
                    // Committed before/after sets remain in task history.
                    self.record_authorization_event(
                        operation.id,
                        operation.surface,
                        &caller,
                        AuditEventStatus::Success,
                        None,
                        None,
                    );
                }
                Ok(())
            }
            Err(denial) => {
                // The CLI prints the refusal itself and the audit row below
                // records the denial, so stay off stderr at the default
                // level; the record remains at debug level.
                tracing::debug!(
                    target: "orbit.authorization",
                    operation = operation.id,
                    provenance = %denial.provenance,
                    granted = %denial.granted,
                    caller_machine_id = denial.remote_caller_machine_id.as_deref(),
                    "governed operation denied"
                );
                let message = if operation.id == TASK_REQUEUE_OPERATION_ID
                    && orbit_common::governance::authorization::agent_context_declared(self.env())
                {
                    format!("{denial}: managed agents cannot requeue blocked tasks")
                } else {
                    denial.to_string()
                };
                self.record_authorization_event(
                    operation.id,
                    operation.surface,
                    &caller,
                    AuditEventStatus::Denied,
                    Some(message.clone()),
                    None,
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
    /// operation did not run. A committed tool mutation makes the other trade
    /// in `finalize_successful_dispatch`: the call stays a success and names
    /// the committed object in `audit_warning`, because an error would not
    /// roll the mutation back.
    pub(crate) fn record_authorization_event(
        &self,
        operation_id: &str,
        surface: OperationSurface,
        caller: &CallerCapabilities,
        status: AuditEventStatus,
        error_message: Option<String>,
        trace_id: Option<&str>,
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
            host: self
                .env()
                .hostname()
                .and_then(std::ffi::OsStr::to_str)
                .map(ToOwned::to_owned),
            pid: std::process::id(),
            session_id: None,
            workspace_id: None,
            caller_machine_id: caller.remote_caller_machine_id().map(ToOwned::to_owned),
            caller_machine_name: None,
            process_machine_id: None,
            process_machine_name: None,
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

        if let Err(error) = self.record_audit_event_with_invocation(
            &params,
            AuditInvocationFields {
                trace_id,
                ..Default::default()
            },
        ) {
            tracing::error!(
                target: "orbit.authorization",
                operation = operation_id,
                "failed to persist authorization audit event: {error}"
            );
        }
    }
}
