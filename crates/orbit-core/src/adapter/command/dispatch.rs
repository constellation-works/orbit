//! Tool dispatch: audit correlation, agent-identity resolution, and the
//! trusted MCP envelope boundary.

use std::cell::Cell;
#[cfg(test)]
use std::cell::RefCell;
use std::path::Path;
use std::time::Instant;

use orbit_common::observability::audit_id::audit_execution_id;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::Store;
use orbit_store::contracts::{AuditEventInsertParams, AuditInvocationFields};
use orbit_tools::{ReservationOwnerContext, ToolContext, ToolExecutionKind};
use orbit_types::identity::{
    normalize_agent_family_for_model, normalize_optional_attribution_label,
};
use orbit_types::policy::Role;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::ToolSessionContext;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::redact_sensitive_env_text;
use crate::runtime::run_input::{
    managed_run_context_from_env, managed_run_context_run_id_from_env,
};
use crate::runtime::tool_exec::{CapabilityEnforcement, populate_filesystem_policy_context};

#[cfg(test)]
pub(super) use crate::runtime::run_input::ORBIT_MANAGED_RUN_CONTEXT_ENV;

/// Where a tool invocation arrived from. Captured in the audit row so a single
/// audit table can attribute tool calls back to their origin (CLI vs MCP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolEntryPoint {
    /// `orbit tool run ...` from the CLI binary.
    Cli,
    /// MCP `tools/call` over `orbit mcp serve`.
    Mcp,
}

impl ToolEntryPoint {
    /// Subcommand value written into the audit row. Distinct values let
    /// downstream queries filter MCP-originated calls without a schema change.
    pub fn audit_subcommand(self) -> &'static str {
        match self {
            ToolEntryPoint::Cli => "run",
            ToolEntryPoint::Mcp => "run-mcp",
        }
    }
}

thread_local! {
    static TOOL_AUDIT_RECORDED: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
thread_local! {
    static TEST_ACTIVITY_TOOLS: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// Restores a test-local activity-tool override when dropped.
///
/// The override is thread-local so tests never need to change process-global
/// environment variables merely to isolate their temporary runtime from a
/// managed executor's inherited activity allowlist.
#[cfg(test)]
pub(crate) struct TestActivityToolsGuard {
    previous: Option<Vec<String>>,
}

#[cfg(test)]
impl Drop for TestActivityToolsGuard {
    fn drop(&mut self) {
        TEST_ACTIVITY_TOOLS.with(|tools| {
            tools.replace(self.previous.take());
        });
    }
}

/// Override the effective managed-agent activity allowlist for this test
/// thread. Production dispatch always reads the inherited activity envelope.
#[cfg(test)]
pub(crate) fn override_activity_tools_for_test(
    allowed_tools: impl IntoIterator<Item = impl Into<String>>,
) -> TestActivityToolsGuard {
    let allowed_tools = allowed_tools.into_iter().map(Into::into).collect();
    let previous = TEST_ACTIVITY_TOOLS.with(|tools| tools.replace(Some(allowed_tools)));
    TestActivityToolsGuard { previous }
}

/// Execute a server-global implementation inside Core's ordinary tool audit
/// boundary without constructing a workspace runtime.
///
/// The callback owns its server-local projection. Core opens only the global
/// audit store, records the supplied invocation context, and preserves the
/// same fail-closed success semantics as runtime-backed tool dispatch.
pub fn execute_global_in_process_tool_dispatch<F>(
    global_root: &Path,
    name: &str,
    input: Value,
    entry_point: ToolEntryPoint,
    session_context: ToolSessionContext,
    dispatch: F,
) -> Result<ToolDispatchOutcome, OrbitError>
where
    F: FnOnce(Value) -> Result<Value, OrbitError>,
{
    execute_tool_dispatch_with_audit_store(
        name,
        input,
        ToolExecutionKind::Mutating,
        ToolDispatchAuditContext {
            agent_override: None,
            model_override: None,
            entry_point,
            session_context: Some(session_context),
        },
        || {
            let audit_db = orbit_config::resolved_audit_db_path(
                &orbit_config::ConfigRoots::global_only(global_root),
            )?;
            Store::open(&audit_db)
        },
        dispatch,
    )
}

/// Mark that the runtime has already persisted an audit row for the current
/// tool invocation on this thread. Higher layers (the CLI `AuditGuard`) call
/// [`take_tool_audit_recorded`] during their own teardown to suppress a
/// duplicate emission. The signal is per-thread and one-shot.
pub fn mark_tool_audit_recorded() {
    TOOL_AUDIT_RECORDED.with(|cell| cell.set(true));
}

/// Read and clear the per-thread tool-audit-recorded flag set by
/// [`mark_tool_audit_recorded`]. Returns `true` if a runtime-level audit row
/// was emitted on this thread since the last call.
pub fn take_tool_audit_recorded() -> bool {
    TOOL_AUDIT_RECORDED.with(|cell| cell.replace(false))
}

/// Outcome of [`OrbitRuntime::execute_tool_command_dispatch`]: the tool's JSON
/// return value plus a flag indicating whether the runtime persisted an audit
/// row for this invocation.
#[derive(Debug)]
pub struct ToolDispatchOutcome {
    pub value: Value,
    pub audit_recorded: bool,
}

struct ToolDispatchAuditContext {
    agent_override: Option<String>,
    model_override: Option<String>,
    entry_point: ToolEntryPoint,
    session_context: Option<ToolSessionContext>,
}

impl OrbitRuntime {
    /// Execute a tool by name and return its JSON value. CLI-callers use this
    /// path; the runtime tags the audit row with [`ToolEntryPoint::Cli`].
    pub fn execute_tool_command(
        &self,
        name: &str,
        input: Value,
        agent_override: Option<String>,
        model_override: Option<String>,
    ) -> Result<Value, OrbitError> {
        self.execute_tool_command_dispatch(
            name,
            input,
            agent_override,
            model_override,
            ToolEntryPoint::Cli,
        )
        .map(|outcome| outcome.value)
    }

    /// Execute a local CLI tool call with a caller-supplied invocation
    /// envelope. The CLI owns machine identity discovery; Core owns dispatch
    /// and persists the resulting audit context.
    pub fn execute_tool_command_with_session_context(
        &self,
        name: &str,
        input: Value,
        agent_override: Option<String>,
        model_override: Option<String>,
        session_context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.execute_tool_command_dispatch_with_session_context(
            name,
            input,
            agent_override,
            model_override,
            ToolEntryPoint::Cli,
            session_context,
        )
        .map(|outcome| outcome.value)
    }

    /// Execute a tool by name and return both the value and whether the
    /// runtime persisted an audit row. Callers that need to suppress a
    /// duplicate higher-level audit emission read `audit_recorded` (also
    /// available out-of-band via [`take_tool_audit_recorded`]).
    pub fn execute_tool_command_dispatch(
        &self,
        name: &str,
        input: Value,
        agent_override: Option<String>,
        model_override: Option<String>,
        entry_point: ToolEntryPoint,
    ) -> Result<ToolDispatchOutcome, OrbitError> {
        self.execute_tool_command_dispatch_with_session_context(
            name,
            input,
            agent_override,
            model_override,
            entry_point,
            ToolSessionContext::default(),
        )
    }

    pub fn execute_tool_command_dispatch_with_session_context(
        &self,
        name: &str,
        input: Value,
        agent_override: Option<String>,
        model_override: Option<String>,
        entry_point: ToolEntryPoint,
        session_context: ToolSessionContext,
    ) -> Result<ToolDispatchOutcome, OrbitError> {
        let audit_session_context = session_context.clone();
        self.execute_tool_dispatch_with(
            name,
            input,
            ToolDispatchAuditContext {
                agent_override: agent_override.clone(),
                model_override: model_override.clone(),
                entry_point,
                session_context: Some(audit_session_context),
            },
            |input| {
                self.ensure_tool_agent_facing(name)?;
                let trusted_env = entry_point != ToolEntryPoint::Mcp || managed_run_context();
                let allowed_tools = if trusted_env {
                    read_activity_tools_from_env()
                } else {
                    Vec::new()
                };
                let (agent_name, model_name) = resolve_agent_identity_for_entry_point(
                    entry_point,
                    agent_override,
                    model_override,
                )?;
                let proc_allowed_programs = if trusted_env {
                    read_proc_allowed_programs_from_env()
                } else {
                    Vec::new()
                };
                let proc_spawn_activity_scoped = managed_run_context();
                let proc_spawn_environment =
                    Some(self.execution_env_policy().agent_subprocess_env(&[]));
                let cwd = std::env::current_dir()
                    .ok()
                    .map(|path| path.to_string_lossy().into_owned());
                let mut tool_context = ToolContext {
                    cwd,
                    session_context,
                    allowed_tools,
                    agent_name,
                    model_name,
                    workspace_root: None,
                    proc_allowed_programs,
                    proc_spawn_environment,
                    proc_spawn_activity_scoped,
                    reservation_owner: reservation_owner_from_env(),
                    ..Default::default()
                };
                if proc_spawn_activity_scoped {
                    populate_filesystem_policy_context(self, &mut tool_context)?;
                }
                let capability_enforcement = match entry_point {
                    ToolEntryPoint::Cli => CapabilityEnforcement::Enforce,
                    ToolEntryPoint::Mcp => CapabilityEnforcement::McpSessionOnly,
                };
                self.run_tool_with_context_and_role_and_capability(
                    name,
                    input,
                    Role::Admin,
                    tool_context,
                    capability_enforcement,
                )
            },
        )
    }

    /// Run an in-process tool implementation inside the same audit boundary
    /// used by registry-backed tool dispatch.
    ///
    /// Transport adapters use this for tools whose implementation deliberately
    /// lives outside the runtime registry. Policy checks belong inside
    /// `dispatch` so a rejection is captured by the resulting audit row.
    pub fn execute_in_process_tool_dispatch<F>(
        &self,
        name: &str,
        input: Value,
        entry_point: ToolEntryPoint,
        session_context: ToolSessionContext,
        dispatch: F,
    ) -> Result<ToolDispatchOutcome, OrbitError>
    where
        F: FnOnce(Value) -> Result<Value, OrbitError>,
    {
        self.execute_tool_dispatch_with(
            name,
            input,
            ToolDispatchAuditContext {
                agent_override: None,
                model_override: None,
                entry_point,
                session_context: Some(session_context),
            },
            dispatch,
        )
    }

    fn execute_tool_dispatch_with<F>(
        &self,
        name: &str,
        input: Value,
        audit: ToolDispatchAuditContext,
        dispatch: F,
    ) -> Result<ToolDispatchOutcome, OrbitError>
    where
        F: FnOnce(Value) -> Result<Value, OrbitError>,
    {
        let execution_kind = self
            .tool_registry()
            .execution_kind(name)
            .unwrap_or(ToolExecutionKind::Mutating);
        execute_tool_dispatch_with_audit_store(
            name,
            input,
            execution_kind,
            audit,
            || self.sqlite_store(),
            dispatch,
        )
    }
}

fn execute_tool_dispatch_with_audit_store<F, S>(
    name: &str,
    input: Value,
    execution_kind: ToolExecutionKind,
    audit: ToolDispatchAuditContext,
    open_audit_store: S,
    dispatch: F,
) -> Result<ToolDispatchOutcome, OrbitError>
where
    F: FnOnce(Value) -> Result<Value, OrbitError>,
    S: FnOnce() -> Result<Store, OrbitError>,
{
    let ToolDispatchAuditContext {
        agent_override,
        model_override,
        entry_point,
        session_context,
    } = audit;
    let start = Instant::now();
    let role_label = audit_role_label_for_entry_point(
        &input,
        agent_override.as_deref(),
        model_override.as_deref(),
        entry_point,
    );
    let working_directory = std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string());
    let audit_context = resolve_audit_context(&input, entry_point, session_context.as_ref());

    // Keep the callback inside the audit boundary so setup, policy, and
    // implementation failures all produce a failure-status row.
    let result = dispatch(input);
    let duration_ms = (start.elapsed().as_millis() as i64).max(1);

    let (status, exit_code, error_message) = match &result {
        Ok(_) => (AuditEventStatus::Success, 0, None),
        // A refusal is not a failure: policy and capability refusals remain
        // queryable as `denied`.
        Err(OrbitError::PolicyDenied(msg) | OrbitError::CapabilityDenied(msg)) => (
            AuditEventStatus::Denied,
            1,
            Some(redact_sensitive_env_text(msg)),
        ),
        Err(
            err @ OrbitError::NotFound {
                kind: NotFoundKind::Tool,
                ..
            },
        ) if entry_point == ToolEntryPoint::Mcp => (
            AuditEventStatus::Denied,
            1,
            Some(redact_sensitive_env_text(&err.to_string())),
        ),
        Err(err) => (
            AuditEventStatus::Failure,
            1,
            Some(redact_sensitive_env_text(&err.to_string())),
        ),
    };

    let params = AuditEventInsertParams {
        execution_id: audit_execution_id("exec"),
        command: "tool".to_string(),
        subcommand: Some(entry_point.audit_subcommand().to_string()),
        tool_name: Some(name.to_string()),
        target_type: Some("tool".to_string()),
        target_id: Some(name.to_string()),
        role: role_label,
        status,
        exit_code,
        duration_ms,
        working_directory,
        arguments_json: None,
        stdout_truncated: None,
        stderr_truncated: None,
        error_message,
        host: std::env::var("HOSTNAME").ok(),
        pid: std::process::id(),
        session_id: None,
        workspace_id: session_context
            .as_ref()
            .and_then(|context| context.workspace_id.clone()),
        caller_machine_id: session_context
            .as_ref()
            .and_then(|context| context.caller_machine_id.clone()),
        caller_host_id: session_context
            .as_ref()
            .and_then(|context| context.caller_host_id.clone()),
        process_machine_id: session_context
            .as_ref()
            .and_then(|context| context.process_machine_id.clone()),
        process_host_id: session_context
            .as_ref()
            .and_then(|context| context.process_host_id.clone()),
        transport: session_context
            .as_ref()
            .and_then(|context| context.transport),
        effective_capabilities: session_context
            .as_ref()
            .map(|context| context.effective_capabilities.clone())
            .unwrap_or_default(),
        origin_session_id: session_context
            .as_ref()
            .and_then(|context| context.origin_session_id.clone()),
        mcp_call_id: session_context
            .as_ref()
            .and_then(|context| context.mcp_call_id.clone()),
        // V1 MCP sessions do not carry execution leases.
        lease_id: None,
        task_id: audit_context.task_id,
        job_run_id: audit_context.job_run_id,
        activity_id: audit_context.activity_id,
        step_index: audit_context.step_index,
    };

    // ORB-10890: the session's self-reported actor rides to the audit row and
    // stops there. `role_label` above was computed without it, and no field of
    // `ToolContext` reads it, so an MCP client naming itself cannot move its
    // own trust label off `unverified`.
    let invocation = AuditInvocationFields {
        trace_id: session_context
            .as_ref()
            .and_then(|context| context.trace_id.as_deref()),
        caller_ip: session_context
            .as_ref()
            .and_then(|context| context.caller_ip.as_deref()),
        self_reported_actor: session_context
            .as_ref()
            .and_then(|context| context.self_reported_actor.as_deref()),
    };
    let audit_write = open_audit_store()
        .and_then(|store| store.insert_audit_event_record_with_invocation(&params, invocation));

    // Claim the row for the runtime the moment it persists, so the CLI
    // `AuditGuard` suppresses its own duplicate emission. This is
    // independent of the tool's own success/failure: the audit row is
    // written for both (a failed call still gets a failure-status row).
    if audit_write.is_ok() {
        mark_tool_audit_recorded();
    }

    // Propagate the tool's own error first. A call that already failed
    // carries no committed mutation to strand, so its error is the
    // authoritative result and the audit-write outcome is irrelevant.
    let value = result?;

    // The tool call succeeded, so any mutation it performed is now
    // committed. Audit persistence is part of that success contract:
    // finding M1 (SECURITY-REVIEW-2026-07-15) — the previous code only
    // `warn!`ed on an audit-write `Err` and still returned the tool's
    // successful value, so an unwritable audit store (disk full, locked
    // db, bad perms) yielded a successful, un-audited mutation with no
    // error surfaced. Fail the call instead so a committed change can
    // never surface without its audit row.
    finalize_successful_dispatch(name, execution_kind, value, audit_write)
}

/// Fold a successful tool call together with its audit-write outcome into the
/// dispatch result.
///
/// On a persisted audit row it returns the value. On a failed audit write it
/// discards the value and returns the error, so a committed mutation can never
/// be surfaced without its audit row (finding M1). The caller is responsible
/// for the per-thread dedup signal, which is set as soon as the row persists
/// regardless of the tool's own outcome.
///
pub(super) fn finalize_successful_dispatch(
    tool_name: &str,
    execution_kind: ToolExecutionKind,
    value: Value,
    audit_write: Result<(), OrbitError>,
) -> Result<ToolDispatchOutcome, OrbitError> {
    match audit_write {
        Ok(()) => Ok(ToolDispatchOutcome {
            value,
            audit_recorded: true,
        }),
        Err(err) if execution_kind == ToolExecutionKind::ReadOnly => {
            tracing::warn!(
                tool = tool_name,
                "read-only tool completed but its audit event could not be persisted: {err}"
            );
            Ok(ToolDispatchOutcome {
                value,
                audit_recorded: false,
            })
        }
        Err(err) => {
            tracing::error!(
                tool = tool_name,
                "failed to persist tool audit event: {err}"
            );
            Err(OrbitError::Store(format!(
                "tool '{tool_name}' completed but its audit row could not be persisted; failing the call so no un-audited change is surfaced: {err}"
            )))
        }
    }
}

/// Trusted audit-correlation fields at the MCP/CLI dispatch seam.
#[derive(Debug, Default, Clone)]
pub struct AuditContext {
    pub task_id: Option<String>,
    pub job_run_id: Option<String>,
    pub activity_id: Option<String>,
    pub step_index: Option<i64>,
}

pub(super) fn resolve_audit_context(
    input: &Value,
    entry_point: ToolEntryPoint,
    session_context: Option<&ToolSessionContext>,
) -> AuditContext {
    fn input_str(input: &Value, key: &str) -> Option<String> {
        input
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    }
    fn env_str(name: &str) -> Option<String> {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    if entry_point == ToolEntryPoint::Mcp {
        // The MCP entry point never reads model-authored tool JSON for
        // correlation; `session_context` is retained in the signature because
        // it is the trusted envelope the caller must hand over to reach it.
        let _ = session_context;
        return trusted_mcp_audit_context();
    }

    AuditContext {
        task_id: input_str(input, "task_id").or_else(|| env_str("ORBIT_TASK_ID")),
        job_run_id: input_str(input, "job_run_id")
            .or_else(|| input_str(input, "run_id"))
            .or_else(|| env_str("ORBIT_RUN_ID")),
        activity_id: input_str(input, "activity_id").or_else(|| env_str("ORBIT_ACTIVITY_ID")),
        step_index: input
            .get("step_index")
            .and_then(Value::as_i64)
            .or_else(|| env_str("ORBIT_STEP_INDEX").and_then(|s| s.parse().ok())),
    }
}

/// Resolve MCP audit correlation exclusively from the managed process
/// envelope. Model-authored tool JSON and session audit metadata are not
/// correlation inputs.
pub fn trusted_mcp_audit_context() -> AuditContext {
    fn env_str(name: &str) -> Option<String> {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    if managed_run_context() {
        AuditContext {
            task_id: env_str("ORBIT_TASK_ID"),
            job_run_id: env_str("ORBIT_RUN_ID"),
            activity_id: env_str("ORBIT_ACTIVITY_ID"),
            step_index: env_str("ORBIT_STEP_INDEX").and_then(|value| value.parse().ok()),
        }
    } else {
        AuditContext::default()
    }
}

pub(super) fn reservation_owner_from_env() -> Option<ReservationOwnerContext> {
    managed_run_context_run_id_from_env().map(|owner_run_id| ReservationOwnerContext {
        owner_metadata_json: Some(
            serde_json::json!({
                "source": "orbit_cli",
            })
            .to_string(),
        ),
        owner_run_id,
    })
}

fn managed_run_context() -> bool {
    managed_run_context_from_env()
}

fn read_agent_identity_from_env() -> (Option<String>, Option<String>) {
    let agent = std::env::var("ORBIT_AGENT_NAME")
        .ok()
        .filter(|s| !s.is_empty());
    let model = std::env::var("ORBIT_AGENT_MODEL")
        .ok()
        .filter(|s| !s.is_empty());
    (agent, model)
}

fn resolve_agent_identity(
    agent_override: Option<String>,
    model_override: Option<String>,
) -> Result<(Option<String>, Option<String>), OrbitError> {
    let (env_agent_name, env_model_name) = read_agent_identity_from_env();
    let has_override = agent_override
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || model_override
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
    let (agent, model) = if has_override {
        (agent_override, model_override)
    } else {
        (env_agent_name, env_model_name)
    };
    let agent = normalize_agent_family_for_model(agent.as_deref(), model.as_deref())?;
    // Tool-call identity crosses a trust boundary: agent-supplied `model`
    // strings are telemetry at best and may be aliases. Persist the canonical
    // family in the model slot for tool dispatch so comparisons never depend
    // on self-reported model text.
    Ok((agent.clone(), agent))
}

fn resolve_agent_identity_for_entry_point(
    entry_point: ToolEntryPoint,
    agent_override: Option<String>,
    model_override: Option<String>,
) -> Result<(Option<String>, Option<String>), OrbitError> {
    if entry_point == ToolEntryPoint::Mcp && !managed_run_context() {
        return Ok((None, None));
    }
    resolve_agent_identity(agent_override, model_override)
}

fn read_proc_allowed_programs_from_env() -> Vec<String> {
    std::env::var("ORBIT_PROC_ALLOWED_PROGRAMS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn read_activity_tools_from_env() -> Vec<String> {
    #[cfg(test)]
    if let Some(allowed_tools) = TEST_ACTIVITY_TOOLS.with(|tools| tools.borrow().clone()) {
        return allowed_tools;
    }

    if std::env::var("ORBIT_TASK_ACTOR_KIND").ok().as_deref() != Some("agent") {
        return Vec::new();
    }
    std::env::var("ORBIT_ACTIVITY_TOOLS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Resolve the audit `role` label for a tool invocation.
///
/// Runtime envelope identity (`ORBIT_AGENT_*`) is authoritative for agent
/// activities and overwrites any self-reported `model` field in tool JSON.
/// Manual CLI/MCP calls without an envelope keep the legacy input/flag
/// precedence.
pub fn audit_role_label(
    input: &Value,
    agent_override: Option<&str>,
    model_override: Option<&str>,
) -> String {
    let (input_agent, input_model) = read_input_identity(input);
    let env_agent = std::env::var("ORBIT_AGENT_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let env_model = std::env::var("ORBIT_AGENT_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let has_input_identity = input_agent.is_some() || input_model.is_some();
    let has_flag_identity = agent_override.is_some_and(|value| !value.trim().is_empty())
        || model_override.is_some_and(|value| !value.trim().is_empty());
    let has_env_identity = env_agent.is_some() || env_model.is_some();
    let (agent, model) = if has_env_identity && !has_flag_identity {
        let agent = normalize_agent_family_for_model(env_agent.as_deref(), env_model.as_deref())
            .ok()
            .flatten()
            .or(env_agent);
        (agent.clone(), agent)
    } else if has_input_identity {
        (input_agent, input_model)
    } else if has_flag_identity {
        (
            agent_override
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
            model_override
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
        )
    } else {
        (env_agent, env_model)
    };
    let agent = normalize_agent_family_for_model(agent.as_deref(), model.as_deref())
        .ok()
        .flatten()
        .or(agent);

    normalize_optional_attribution_label(model.as_deref().or(agent.as_deref()), model.as_deref())
        .unwrap_or_else(|| "agent".to_string())
}

/// Resolve the audit role with the MCP trust boundary applied. Standalone MCP
/// calls are always `unverified`; an authenticated managed envelope may use
/// only its engine-provided identity and never caller JSON/flags.
pub fn audit_role_label_for_entry_point(
    input: &Value,
    agent_override: Option<&str>,
    model_override: Option<&str>,
    entry_point: ToolEntryPoint,
) -> String {
    if entry_point != ToolEntryPoint::Mcp {
        return audit_role_label(input, agent_override, model_override);
    }
    if !managed_run_context() {
        return "unverified".to_string();
    }

    let env_agent = std::env::var("ORBIT_AGENT_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let env_model = std::env::var("ORBIT_AGENT_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let agent = normalize_agent_family_for_model(env_agent.as_deref(), env_model.as_deref())
        .ok()
        .flatten()
        .or(env_agent);
    normalize_optional_attribution_label(
        agent.as_deref().or(env_model.as_deref()),
        env_model.as_deref(),
    )
    .unwrap_or_else(|| "unverified".to_string())
}

fn read_input_identity(input: &Value) -> (Option<String>, Option<String>) {
    if let Value::Object(map) = input {
        let agent = map
            .get("agent")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let model = map
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        (agent, model)
    } else {
        (None, None)
    }
}
