//! The audit row of a governed dashboard operation whose handler still
//! authorizes and records it itself.
//!
//! An operation that runs through an app's `exec` is recorded there. The
//! handlers left here (script grants, routine breakers, distributed drains)
//! move to `exec` with their subjects.

use std::borrow::Cow;
use std::ffi::OsStr;
use std::sync::Arc;
use std::time::Instant;

use orbit_common::Env;
use orbit_common::governance::authorization::{AuthorizationDenial, CallerCapabilities};
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_core::{AuditEventInsertParams, OrbitRuntime};
use orbit_types::telemetry::AuditEventStatus;
use serde_json::Value;

/// One governed dashboard operation, ready to audit any of its outcomes.
///
/// The invariant part (runtime, workspace, operation, target, machine and
/// arguments) is carried once; each method records exactly one outcome, so
/// a caller and a denial can never be recorded together or both be absent.
/// `machine_name` defaults to empty.
pub(super) struct OperationAudit<'a> {
    runtime: &'a Arc<OrbitRuntime>,
    host: Option<String>,
    workspace: &'a str,
    operation: &'a str,
    target: Cow<'a, str>,
    machine_name: &'a str,
    arguments: Value,
}

impl<'a> OperationAudit<'a> {
    pub(super) fn new(
        runtime: &'a Arc<OrbitRuntime>,
        env: &Env,
        workspace: &'a str,
        operation: &'a str,
        target: impl Into<Cow<'a, str>>,
        arguments: Value,
    ) -> Self {
        Self {
            runtime,
            host: env.hostname().and_then(OsStr::to_str).map(str::to_string),
            workspace,
            operation,
            target: target.into(),
            machine_name: "",
            arguments,
        }
    }

    pub(super) fn with_machine_name(mut self, machine_name: &'a str) -> Self {
        self.machine_name = machine_name;
        self
    }

    pub(super) async fn denied(&self, denial: &AuthorizationDenial, started: Instant) {
        self.record(None, Some(denial), None, started).await;
    }

    pub(super) async fn failed(
        &self,
        caller: &CallerCapabilities,
        failure: &str,
        started: Instant,
    ) {
        self.record(Some(caller), None, Some(failure), started)
            .await;
    }

    pub(super) async fn succeeded(&self, caller: &CallerCapabilities, started: Instant) {
        self.record(Some(caller), None, None, started).await;
    }

    pub(super) async fn settled(
        &self,
        caller: &CallerCapabilities,
        failure: Option<&str>,
        started: Instant,
    ) {
        self.record(Some(caller), None, failure, started).await;
    }

    async fn record(
        &self,
        caller: Option<&CallerCapabilities>,
        denial: Option<&AuthorizationDenial>,
        failure: Option<&str>,
        started: Instant,
    ) {
        let operation = self.operation;
        let target = &*self.target;
        let status = if denial.is_some() {
            AuditEventStatus::Denied
        } else if failure.is_some() {
            AuditEventStatus::Failure
        } else {
            AuditEventStatus::Success
        };
        let capabilities = caller
            .map(|caller| caller.grants().clone())
            .unwrap_or_default();
        let provenance = caller
            .map(|caller| caller.provenance().to_string())
            .or_else(|| denial.map(|denial| denial.provenance.to_string()));
        let params = AuditEventInsertParams {
            execution_id: audit_execution_id("dashboard-operations"),
            command: "dashboard.operations".to_string(),
            subcommand: provenance,
            tool_name: None,
            target_type: Some(operation.to_string()),
            target_id: Some(target.to_string()),
            role: "operator".to_string(),
            status,
            exit_code: i32::from(status != AuditEventStatus::Success),
            duration_ms: i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX),
            working_directory: self
                .runtime
                .workspace_runtime_binding()
                .map(|binding| binding.repo_root.display().to_string())
                .unwrap_or_else(|| self.runtime.shared_root().display().to_string()),
            arguments_json: serde_json::to_string(&self.arguments).ok(),
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: denial
                .map(ToString::to_string)
                .or_else(|| failure.map(str::to_string)),
            host: self.host.clone(),
            pid: std::process::id(),
            session_id: None,
            workspace_id: Some(self.workspace.to_string()),
            caller_machine_id: None,
            caller_machine_name: Some(self.machine_name.to_string()),
            process_machine_id: None,
            process_machine_name: Some(self.machine_name.to_string()),
            transport: None,
            effective_capabilities: capabilities,
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: None,
            job_run_id: None,
            activity_id: None,
            step_index: None,
        };
        // A SQLite insert can wait out the busy timeout; keep it off the async
        // worker like every other store access in this crate.
        let writer = Arc::clone(self.runtime);
        let result = tokio::task::spawn_blocking(move || writer.record_audit_event(&params)).await;
        let error = match result {
            Ok(Ok(())) => return,
            Ok(Err(error)) => error.to_string(),
            Err(join_error) => join_error.to_string(),
        };
        tracing::error!(
            operation,
            target,
            error = %error,
            "failed to persist dashboard operation audit",
        );
    }
}
