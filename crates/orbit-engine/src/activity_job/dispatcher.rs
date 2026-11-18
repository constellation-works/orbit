use std::sync::Arc;

use orbit_types::workflow::activity_job::V2AuditEventKind;
use orbit_types::workflow::activity_job::{ActivityV2Spec, AgentLoopSpec, DeterministicSpec};

use crate::context::RuntimeHost;
use orbit_common::{OrbitError, PrivateVcsFailureClass};
use orbit_tools::{
    DeterministicStepPrograms, FsAuditLogger, FsCallEvent, FsCallEventKind, ToolCaller,
};
use orbit_types::tool::McpCapability;
use orbit_types::workflow::DeterministicAction;
use serde_json::Value;

use super::audit_writer::V2AuditWriter;
use super::cli_runner::{run_cli_backend_for_step, task_id_from_input};

// Compatibility paths for existing callers; migrate them before retiring this shim.
pub use crate::context::dispatch_contract::{
    DispatchError, DispatchInvocationTrace, DispatchOutcome, LinuxRuntimeWriteAuthority,
    ResolvedCliExecutor, ResolvedSandbox, ResolvedShellExecutor, SandboxMask,
    dispatch_error_to_orbit,
};

/// Input bundle for a single v2 activity dispatch.
pub struct V2DispatchInput<'a> {
    pub activity_name: &'a str,
    pub spec: &'a ActivityV2Spec,
    pub fs_profile: Option<&'a str>,
    pub input: Value,
    pub audit: Arc<V2AuditWriter>,
    pub run_id: &'a str,
    /// Runtime host for agent_loop + deterministic paths. A `None` host is only
    /// valid for callers that never dispatch a host-backed activity; host-backed
    /// specs return `DispatchError::HostRequired` when it is absent.
    pub host: Option<&'a dyn RuntimeHost>,
}

/// Dispatch a v2 activity by type. Emits §7 activity.started/finished
/// events around the per-type runner and nests the runner's events beneath.
pub fn dispatch_v2_activity(input: V2DispatchInput<'_>) -> Result<DispatchOutcome, DispatchError> {
    dispatch_v2_activity_inner(input, None, true, None)
}

/// Dispatch a pipeline step whose id may differ from the catalog activity it
/// targets.
///
/// `input.activity_name` stays the step id, which labels the activity events
/// and failure messages. `target_activity` is the catalog name: an agent
/// loop's tool policy, `ORBIT_ACTIVITY_NAME`, tool denials and plugin broker
/// identity are keyed by it, so a step `review` targeting
/// `agent_review_repair` is authorized as the reviewer. `None` (an inline
/// spec, which has no catalog name) falls back to the step id. An admitted
/// review's `wall_clock_bound` takes precedence over workspace activity policy.
pub(crate) fn dispatch_v2_target_activity(
    input: V2DispatchInput<'_>,
    target_activity: Option<&str>,
    wall_clock_bound: Option<u64>,
) -> Result<DispatchOutcome, DispatchError> {
    dispatch_v2_activity_inner(input, target_activity, true, wall_clock_bound)
}

pub(crate) fn dispatch_v2_activity_without_run_id_injection(
    input: V2DispatchInput<'_>,
) -> Result<DispatchOutcome, DispatchError> {
    dispatch_v2_activity_inner(input, None, false, None)
}

fn dispatch_v2_activity_inner(
    input: V2DispatchInput<'_>,
    target_activity: Option<&str>,
    inject_run_id_into_input: bool,
    wall_clock_bound: Option<u64>,
) -> Result<DispatchOutcome, DispatchError> {
    let activity_input = if inject_run_id_into_input {
        inject_run_id(&input.input, input.run_id)
    } else {
        input.input.clone()
    };
    let spec = input.spec;
    let activity_type = match spec {
        ActivityV2Spec::AgentLoop(_) => "agent_loop",
        ActivityV2Spec::Deterministic(_) => "deterministic",
    };

    let activity_event_id = input
        .audit
        .emit(
            orbit_types::workflow::activity_job::V2AuditEventKind::ActivityStarted {
                activity_name: input.activity_name.to_string(),
                activity_type: activity_type.to_string(),
            },
        )
        .map_err(|err| DispatchError::AuditFailed(format!("{err:?}")))?;
    input.audit.push_parent_lossy(activity_event_id);

    let result = match spec {
        ActivityV2Spec::AgentLoop(spec) => match input.host {
            Some(host) => run_agent_loop_activity(
                host,
                input.activity_name,
                target_activity.unwrap_or(input.activity_name),
                spec,
                input.run_id,
                input.audit.clone(),
                &activity_input,
                input.fs_profile,
                wall_clock_bound,
            ),
            None => Err(DispatchError::HostRequired("agent_loop")),
        },
        ActivityV2Spec::Deterministic(spec) => match input.host {
            Some(host) => run_deterministic(
                host,
                input.run_id,
                input.activity_name,
                spec,
                input.fs_profile,
                input.audit.clone(),
                &activity_input,
            ),
            None => Err(DispatchError::HostRequired("deterministic")),
        },
    };

    input.audit.pop_parent_lossy();
    let outcome_str = match &result {
        Ok(o) if o.success => "success",
        Ok(_) => "failed",
        Err(
            DispatchError::ReviewEvidenceHold(_)
            | DispatchError::ForgeUnavailableHold(_)
            | DispatchError::DeliveryRefused { .. }
            | DispatchError::FootprintUnavailable(_),
        ) => "held",
        Err(DispatchError::ChildRunCancelled { .. }) => "cancelled",
        Err(_) => "error",
    };
    input.audit.emit_lossy(
        orbit_types::workflow::activity_job::V2AuditEventKind::ActivityFinished {
            activity_name: input.activity_name.to_string(),
            outcome: outcome_str.to_string(),
        },
    );

    result
}

pub(crate) fn inject_run_id(input: &Value, run_id: &str) -> Value {
    let Value::Object(map) = input else {
        return input.clone();
    };
    if map.contains_key("run_id") {
        // An explicit `run_id` is a worktree identity token, not a run
        // record. The admitted job still owns execution authority; expose it
        // as `job_run_id` when the caller did not.
        if map.contains_key("job_run_id") {
            return input.clone();
        }
        let mut augmented = map.clone();
        augmented.insert("job_run_id".to_string(), Value::String(run_id.to_string()));
        return Value::Object(augmented);
    }

    let mut augmented = map.clone();
    augmented.insert("run_id".to_string(), Value::String(run_id.to_string()));
    Value::Object(augmented)
}

/// Name the dispatching step in a deterministic action's input.
///
/// [ORB-10971] An action that persists something into the run's own state —
/// a child dispatch checkpoint, say — needs to say *which* step produced it,
/// and `run_id` alone cannot. Scoped to the deterministic path: agent-loop
/// envelopes are a provider-facing contract and are deliberately left alone.
/// An input that already carries `step_id` wins, so a job asset can still
/// address a different step explicitly.
fn inject_step_id(input: &Value, step_id: &str) -> Value {
    let Value::Object(map) = input else {
        return input.clone();
    };
    if map.contains_key("step_id") {
        return input.clone();
    }

    let mut augmented = map.clone();
    augmented.insert("step_id".to_string(), Value::String(step_id.to_string()));
    Value::Object(augmented)
}

fn run_deterministic(
    host: &dyn RuntimeHost,
    run_id: &str,
    activity_name: &str,
    spec: &DeterministicSpec,
    fs_profile: Option<&str>,
    audit: Arc<V2AuditWriter>,
    input: &Value,
) -> Result<DispatchOutcome, DispatchError> {
    let mut tool_context = host.tool_context_for_activity(
        Some(run_id),
        fs_profile,
        Some(v2_fs_audit_logger(audit.clone())),
        None,
    );
    tool_context
        .session_context
        .effective_capabilities
        .insert(McpCapability::Runner);
    // No agent chose this call: the asset fixed it. A plugin tool it reaches
    // may spawn what the operator granted that plugin, which the dispatching
    // action re-reads from the grants witness; `proc.spawn` itself stays on
    // the empty, fail-closed list above. A deterministic activity declares no
    // program allowlist of its own, so there is nothing to intersect with
    // [ORB-13270].
    tool_context.caller = ToolCaller::DeterministicStep(DeterministicStepPrograms::default());
    // The dispatcher names the task this step serves from the run's own input, the
    // same value a CLI agent step exports as `ORBIT_TASK_ID`. A plugin reads
    // it as host-attested context; the tool's `input`/`args` never feed it.
    if let Some(binding) = tool_context.activity_binding.as_mut() {
        binding.task_id = task_id_from_input(input).map(ToOwned::to_owned);
    }
    let state_context = crate::executor::automation::StateExecutionContext {
        run_id: input
            .get("run_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        fs_profile: fs_profile.map(ToOwned::to_owned),
        ..crate::executor::automation::StateExecutionContext::default()
    };
    let output = match DeterministicAction::parse(&spec.action) {
        Some(DeterministicAction::Engine(
            orbit_types::workflow::EngineDeterministicAction::LocalShell,
        )) => crate::executor::automation::dispatch_local_shell(
            host,
            &spec.config,
            input,
            Some(&state_context),
        )?,
        Some(DeterministicAction::Engine(action)) => {
            crate::executor::automation::execute_engine_action(
                host,
                action,
                &spec.config,
                input,
                Some(&state_context),
            )
            .map_err(|error| match error {
                OrbitError::DeliveryRefused { refusal, message } => {
                    DispatchError::DeliveryRefused {
                        action: spec.action.clone(),
                        refusal,
                        message,
                    }
                }
                OrbitError::RecoverableVcsConflict(conflict) => {
                    DispatchError::RecoverableVcsConflict(conflict)
                }
                OrbitError::CandidateAbsorbed(candidate) => DispatchError::CandidateAbsorbed {
                    message: format!(
                        "deterministic action `{}` failed: execution failed: {candidate}",
                        spec.action
                    ),
                    candidate,
                },
                OrbitError::TaskCompletionLiveRun { task_id, run_id } => {
                    DispatchError::TaskCompletionLiveRun { task_id, run_id }
                }
                OrbitError::ForgeUnavailable(failure) => {
                    DispatchError::ForgeUnavailableHold(Box::new(failure.hold))
                }
                error
                    if error.private_vcs_failure().is_some_and(|failure| {
                        failure.class == PrivateVcsFailureClass::CompletionObservationUnavailable
                    }) =>
                {
                    DispatchError::CompletionObservationUnavailable {
                        action: spec.action.clone(),
                        message: error.to_string(),
                    }
                }
                error
                    if error
                        .step_failure_code()
                        .is_some_and(|code| code.is_owner_route_unavailable()) =>
                {
                    DispatchError::OwnerRouteUnavailable {
                        action: spec.action.clone(),
                        message: error.to_string(),
                    }
                }
                error => DispatchError::DeterministicActionFailed {
                    action: spec.action.clone(),
                    message: error.to_string(),
                },
            })?
        }
        Some(DeterministicAction::Core(_)) | None => host.run_deterministic(
            &spec.action,
            &spec.config,
            &inject_step_id(input, activity_name),
            tool_context,
        )?,
    };
    Ok(DispatchOutcome {
        success: true,
        output,
        message: None,
        failure_code: None,
        invocation: None,
    })
}

#[allow(clippy::too_many_arguments)]
fn run_agent_loop_activity(
    host: &dyn RuntimeHost,
    step_id: &str,
    target_activity: &str,
    spec: &AgentLoopSpec,
    run_id: &str,
    audit: Arc<V2AuditWriter>,
    input: &Value,
    fs_profile: Option<&str>,
    wall_clock_bound: Option<u64>,
) -> Result<DispatchOutcome, DispatchError> {
    run_cli_backend_for_step(
        host,
        spec,
        step_id,
        target_activity,
        run_id,
        audit,
        input,
        fs_profile,
        wall_clock_bound,
    )
    .map(|outcome| label_failure_with_step(step_id, outcome))
}

/// [ORB-10449] Prefix a failing CLI agent-loop message with the step that
/// produced it.
///
/// A run surfaces only its terminal message, and the executor's fallback
/// (`step `<id>` completed with success=false`) is used only when the step
/// reports no message at all. So a step that *does* report one was previously
/// anonymous in the run record — an operator saw the symptom without the
/// origin. Naming the step here keeps that fix in one place for every CLI
/// agent-loop failure mode (timeout, nonzero exit, protocol violation,
/// invalid envelope).
fn label_failure_with_step(activity_name: &str, mut outcome: DispatchOutcome) -> DispatchOutcome {
    if !outcome.success
        && let Some(message) = outcome.message.take()
    {
        outcome.message = Some(format!("step `{activity_name}`: {message}"));
    }
    outcome
}

struct V2FsAuditLogger {
    audit: Arc<V2AuditWriter>,
}

impl FsAuditLogger for V2FsAuditLogger {
    fn emit(&self, event: FsCallEvent) -> Result<(), OrbitError> {
        let kind = match event.kind {
            FsCallEventKind::Request => V2AuditEventKind::FsCallRequest {
                profile: event.profile,
                op: event.op,
                path: event.path,
                allowed: event.allowed,
                matched_rule: event.matched_rule,
            },
            FsCallEventKind::Result => V2AuditEventKind::FsCallResult {
                profile: event.profile,
                op: event.op,
                path: event.path,
                allowed: event.allowed,
                matched_rule: event.matched_rule,
            },
            FsCallEventKind::Denied => V2AuditEventKind::FsCallDenied {
                profile: event.profile,
                op: event.op,
                path: event.path,
                allowed: event.allowed,
                matched_rule: event.matched_rule,
            },
        };

        self.audit
            .emit(kind)
            .map(|_| ())
            .map_err(|error| OrbitError::Execution(format!("audit write failed: {error}")))
    }
}

pub(crate) fn v2_fs_audit_logger(audit: Arc<V2AuditWriter>) -> Arc<dyn FsAuditLogger> {
    Arc::new(V2FsAuditLogger { audit })
}
