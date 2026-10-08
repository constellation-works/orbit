//! Provider subprocess dispatch for `backend: cli`: argv and environment
//! composition, the run's plugin broker, and spawn supervision.

use std::cell::Cell;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use orbit_agent::{Agent, AgentConfig, AgentOperation, AgentRequest};
use orbit_common::process::identity::process_start_identity_token;
use orbit_common::process::stopped_descendants::{StoppedDescendant, stopped_descendant_threshold};
use orbit_common::security::child_env::{
    ACTIVITY_DEADLINE_ENV, MCP_MANAGED_REGISTRY_ROOT_ENV, MCP_MANAGED_WORKSPACE_ENV,
};
use orbit_common::security::redaction::argv_redactor;
use orbit_tools::plugin::BrokeredCaller;
use orbit_types::workflow::activity_job::{AgentLoopSpec, V2AuditEventKind};
use orbit_types::workflow::{ActivityToolDenyPolicy, ExecutorSandboxKind};
use serde_json::Value;

use crate::context::{PluginBrokerRun, ProvenanceEnv, provenance_env};
use crate::executor::automation::vcs::git::{GitTimeoutBudget, GitTimeoutBudgetGuard};

use super::super::super::audit_writer::V2AuditWriter;
use super::super::super::dispatcher::{DispatchError, DispatchOutcome};
use super::super::super::workspace::{WorktreeBoundaryGuard, validate_declared_worktree_pair};
use super::super::argv::{
    apply_provider_runtime_arg_fixups, apply_provider_static_arg_fixups,
    apply_trusted_host_provider_sandbox, codex_mcp_server_launch_args, neutralize_inner_sandbox,
    try_audit_argv_for_dispatch,
};
use super::super::envelope::{cli_agent_envelope_json, task_id_from_input, task_ids_from_input};
use super::super::inspection_tools::prepare_inspection_tools;
use super::super::launcher::{orbit_tool_env, resolve_provider_launcher};
use super::super::plugin_broker::RunPluginBroker;
use super::super::spawn::{CODEX_CA_CERTIFICATE_ENV, SSL_CERT_FILE_ENV, SpawnError};
use super::super::stdout_preview::{PROGRESS_MESSAGE_LIMIT_BYTES, bounded_assistant_message};
use super::super::supervisor::{
    OutputProgress, ProgressReporter, SpawnTraceContext, SpawnWithTimeoutRequest,
    StoppedDescendantReporter, spawn_for_supervision, spawn_with_timeout,
};
use super::completion::{ProviderExit, project_completion};
use super::policy::{
    ActivityToolGrant, activity_policy_env, drop_inherited_policy_env, resolve_activity_tool_grant,
};
use super::prepare::{
    SourceCheckout, activity_tool_context, invocation_timeout_seconds, prepare_dispatch_sandbox,
    prepare_source_checkout, require_conflict_recovery_target, resolve_dispatch_sandbox,
    trusted_host_admission,
};
use crate::context::RuntimeHost;

/// How often a running provider's stdout is sampled for progress. Bounds the
/// staleness of `last_activity_at` and the progress rows one invocation writes.
const PROVIDER_PROGRESS_INTERVAL: Duration = Duration::from_secs(10);

/// The provider's wall-clock deadline, as the `ORBIT_ACTIVITY_DEADLINE_UNIX_MS`
/// envelope entry. A deadline that cannot be represented as a `SystemTime` has
/// no entry: stamping a past value would read as an exhausted budget downstream.
fn activity_deadline_env(wall_clock_timeout: Duration) -> Option<(String, String)> {
    let since_epoch = SystemTime::now()
        .checked_add(wall_clock_timeout)?
        .duration_since(UNIX_EPOCH)
        .ok()?;
    let deadline_ms = u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX);
    Some((ACTIVITY_DEADLINE_ENV.to_string(), deadline_ms.to_string()))
}

pub fn run_cli_backend(
    host: &dyn RuntimeHost,
    spec: &AgentLoopSpec,
    activity_name: &str,
    run_id: &str,
    audit: Arc<V2AuditWriter>,
    input: &Value,
    fs_profile: Option<&str>,
) -> Result<DispatchOutcome, DispatchError> {
    run_cli_backend_for_step(
        host,
        spec,
        activity_name,
        activity_name,
        run_id,
        audit,
        input,
        fs_profile,
    )
}

/// Run a catalog activity on behalf of a pipeline step whose id differs from
/// the activity name. Policy and broker authorization use `activity_name`;
/// audit labels and worktree-boundary reports use `step_id`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_cli_backend_for_step(
    host: &dyn RuntimeHost,
    spec: &AgentLoopSpec,
    step_id: &str,
    activity_name: &str,
    run_id: &str,
    audit: Arc<V2AuditWriter>,
    input: &Value,
    fs_profile: Option<&str>,
) -> Result<DispatchOutcome, DispatchError> {
    let budget = GitTimeoutBudget::from_input(input)
        .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))?;
    let _git_timeout_budget = GitTimeoutBudgetGuard::install(budget);
    let provider = spec.provider.as_str().to_string();
    let trusted_host = trusted_host_admission(spec, activity_name, input)?;
    // Reject an unsupported inspection provider before executor lookup can
    // classify it as a transient missing-CLI error.
    prepare_inspection_tools(&provider, input, fs_profile, &[])?;
    let mut cli_executor = host.resolve_cli_executor(&provider)?;
    let inspection_tools =
        prepare_inspection_tools(&provider, input, fs_profile, &cli_executor.args)?;
    let timeout_seconds = invocation_timeout_seconds(spec, trusted_host.as_ref(), input);
    let wall_clock_timeout = Duration::from_secs(timeout_seconds);

    let task_ids = task_ids_from_input(input);
    let task_id = task_id_from_input(input);
    require_conflict_recovery_target(activity_name, &task_ids, input, run_id)?;
    let ActivityToolGrant {
        tool_policy,
        tool_disallow_list,
        activity_tools,
    } = resolve_activity_tool_grant(host, spec, activity_name, input, &task_ids)?;

    // §6 allowlist-advisory event — emitted once per invocation before the
    // subprocess starts so a reviewer can see the enforcement gap at a glance.
    audit.emit_lossy(V2AuditEventKind::ToolAllowlistHarnessDelegated {
        provider: provider.clone(),
        task_id: task_id.map(ToOwned::to_owned),
        task_ids: task_ids.clone(),
        requested_tools: activity_tools.requested_tools.clone(),
        effective_tools: activity_tools.effective_tools.clone(),
        tools: activity_tools.effective_tools.clone(),
        tool_policy: Some(tool_policy),
        tool_disallow_list: tool_disallow_list.clone(),
    });

    let task_ctx = host.task_context_for_agent_input(input)?;
    let tool_ctx = activity_tool_context(host, spec, run_id, fs_profile, &provider);
    // A shipment pipeline renders the assigned checkout twice: once as the
    // child cwd and once inside the agent contract. Validate that pair against
    // the registered primary before sandbox construction or provider spawn.
    let declared_worktree_pair = validate_declared_worktree_pair(
        input,
        task_ctx.as_ref(),
        run_id,
        &provider,
        tool_ctx.workspace_root.as_deref(),
    )?;
    let SourceCheckout {
        source_cwd,
        inspection,
        inspection_input,
        inspection_task_ctx,
        subprocess_cwd,
        subprocess_cwd_string,
    } = prepare_source_checkout(
        input,
        task_ctx.as_ref(),
        tool_ctx.workspace_root.as_deref(),
        fs_profile,
    )?;
    let resolved_sandbox = resolve_dispatch_sandbox(
        host,
        &provider,
        fs_profile,
        subprocess_cwd.as_deref(),
        trusted_host.as_ref(),
    )?;
    let prepared_sandbox =
        prepare_dispatch_sandbox(trusted_host.as_ref(), resolved_sandbox.as_ref())?;
    let sandbox = prepared_sandbox.effective;

    let mut envelope_spec = spec.clone();
    if let Some(tools) = &inspection_tools {
        envelope_spec.instruction.push_str("\n\n");
        envelope_spec.instruction.push_str(tools.instruction);
    }
    let envelope_json = cli_agent_envelope_json(
        &envelope_spec,
        run_id,
        inspection_input.as_ref().unwrap_or(input),
        inspection_task_ctx.as_ref().or(task_ctx.as_ref()),
        &activity_tools.requested_tools,
        &activity_tools.effective_tools,
    )?;

    let mut provider_config = host.provider_cli_config(&provider);
    if trusted_host.is_some() {
        apply_trusted_host_provider_sandbox(&provider, input, &mut provider_config);
    }

    // Provider-specific static-arg fixups that are independent of whether the
    // outer sandbox is active. Today this only rewrites Claude's `--debug-file`
    // value to an absolute path under the writable claude state dir, so the
    // log lands somewhere `denyModify: .orbit/**` does not block. See
    // T20260505-22.
    apply_provider_static_arg_fixups(&provider, &mut cli_executor.args);

    // Inner-sandbox neutralization. When orbit-exec wraps the CLI we are the
    // single source of truth for filesystem enforcement; the agent's own
    // sandbox flag would either double-encode the same constraint or
    // contradict it. We neutralize per-provider rather than layering:
    //   - codex: pin `--sandbox danger-full-access` so codex behaves
    //     transparently inside our outer sandbox.
    //   - gemini: drop `-s` / `--sandbox` from the executor's static args.
    //   - claude: nothing to do; claude has no OS-level sandbox flag.
    // An explicit executor opt-out must not silently restore the provider's
    // inner sandbox when preparation selects a bare process.
    let explicitly_off = resolved_sandbox
        .as_ref()
        .is_some_and(|sandbox| sandbox.kind == ExecutorSandboxKind::Off);
    if sandbox.is_some() || explicitly_off {
        neutralize_inner_sandbox(&provider, &mut provider_config, &mut cli_executor.args);
    }

    // Config parse / agent construction failures are deterministic — the
    // same spec fails identically on every attempt, so they are classified
    // permanent and skip the step retry wrapper (ORB-10006).
    let config = AgentConfig::from_cli_config(
        cli_executor.command.clone(),
        spec.model.as_deref(),
        &provider_config,
    )
    .and_then(|config| config.with_reasoning_effort(spec.reasoning_effort))
    .map_err(|err| DispatchError::CliInvocationPermanent(format!("agent config: {err}")))?;
    let agent = Agent::new(&config)
        .map_err(|err| DispatchError::CliInvocationPermanent(format!("agent build: {err}")))?;

    let agent_req = AgentRequest {
        operation: AgentOperation::Activity {
            activity_id: "v2_cli_backend".to_string(),
        },
        envelope_json,
        verbose: false,
    };

    // `invoke` only renders the argv/stdin for the subprocess (nothing has
    // executed yet) — failures here are deterministic request-shaping errors.
    let (invocation, _trace) = agent
        .invoke(agent_req)
        .map_err(|err| DispatchError::CliInvocationPermanent(format!("agent invoke: {err}")))?;
    let model = agent.model_name().map(str::to_string);
    let resolved_program =
        resolve_provider_launcher(&provider, &invocation.program, subprocess_cwd.as_deref())
            .map_err(|err| DispatchError::CliInvocationPermanent(err.message))?;
    let orbit_env =
        orbit_tool_env().map_err(|error| DispatchError::CliInvocationPermanent(error.message))?;

    let mut subprocess_args = Vec::with_capacity(cli_executor.args.len() + invocation.args.len());
    subprocess_args.extend(cli_executor.args.iter().cloned());
    subprocess_args.extend(invocation.args.iter().cloned());
    if provider == "codex" {
        let orbit_bin = orbit_env
            .iter()
            .find(|(name, _)| name == "ORBIT_BIN")
            .map(|(_, value)| value.as_str())
            .ok_or_else(|| {
                DispatchError::CliInvocationPermanent("managed ORBIT_BIN missing".into())
            })?;
        subprocess_args.extend(codex_mcp_server_launch_args(orbit_bin).map_err(|error| {
            DispatchError::CliInvocationPermanent(format!("encode codex MCP command: {error}"))
        })?);
    }
    // Combined executor + transport argv is the only place that can honor a
    // custom `--print-timeout` without duplicating it, and the remaining
    // spawn deadline is known here. [ORB-11337]
    let print_timeout =
        apply_provider_runtime_arg_fixups(&provider, &mut subprocess_args, wall_clock_timeout);
    if let Some(tools) = inspection_tools {
        subprocess_args.extend(tools.args);
    }

    // The audit argv reflects what actually runs. Under sandbox-exec the
    // parent is `<trusted sandbox-exec> -f <profile.sb> <program> <args...>`;
    // under bare exec it's `<program> <args...>`. The redactor still scrubs
    // the child's program name + args so secrets in argv stay redacted.
    let redaction = argv_redactor();
    let audit_argv = try_audit_argv_for_dispatch(
        &resolved_program,
        &subprocess_args,
        sandbox,
        subprocess_cwd.as_deref(),
    )
    .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))?;
    let argv_redacted: Vec<String> = audit_argv.iter().map(|a| redaction.apply_str(a)).collect();

    let stdin_blob_ref = audit.write_blob(&invocation.stdin);

    // L-0095: Provider cwd is advisory; enforce the linked-worktree postcondition.
    // Inspection owns its temporary checkout separately; retain the original
    // source pair here so task-worktree ownership validation stays unchanged.
    // Snapshot both sides of a linked-worktree invocation immediately before
    // provider spawn. `tool_ctx.workspace_root` is the registered primary
    // checkout; `subprocess_cwd` is the canonical assigned worktree. Direct
    // invocations where those resolve to the same checkout remain unchanged.
    let mut worktree_boundary = WorktreeBoundaryGuard::capture(
        input,
        task_ctx.as_ref(),
        run_id,
        &provider,
        source_cwd.as_deref(),
        tool_ctx.workspace_root.as_deref(),
        declared_worktree_pair.as_ref(),
    )?
    // A violation's full fingerprints belong in the run's blob store, not in
    // the error string every downstream reader copies [ORB-12467].
    .map(|boundary| {
        boundary
            .with_audit(Arc::clone(&audit))
            .with_activity(step_id)
    });

    if activity_name == "pr_conflict_recovery" {
        let boundary = worktree_boundary.as_mut().ok_or_else(|| {
            DispatchError::CliInvocationPermanent(
                "conflict recovery requires a validated assigned worktree; refusing primary-checkout execution".to_string(),
            )
        })?;
        boundary.authorize_rebase_completion(host, input)?;
    }

    if let Some(admission) = &trusted_host {
        tracing::warn!(
            target: "orbit.trusted_host",
            run_id,
            activity_name,
            step_id,
            provider = %provider,
            authorized_by = %admission.authorized_by,
            authorizer_provenance = %admission.authorizer_provenance,
            caller_machine_id = admission.caller_machine_id.as_deref(),
            workspace_path = %admission.workspace_path,
            cwd = %admission.cwd,
            "starting an operator-admitted provider subprocess outside the executor sandbox"
        );
        audit.emit_lossy(V2AuditEventKind::TrustedHostExecutionAdmitted {
            provider: provider.clone(),
            activity_name: step_id.to_string(),
            authorized_by: admission.authorized_by.clone(),
            authorizer_provenance: admission.authorizer_provenance.clone(),
            caller_machine_id: admission.caller_machine_id.clone(),
            authorized_at: admission.authorized_at.clone(),
            workspace_path: admission.workspace_path.clone(),
            cwd: admission.cwd.clone(),
        });
    }

    let model_redacted = agent.model_name().map(|m| redaction.apply_str(m));
    audit.emit_lossy(V2AuditEventKind::CliInvocationStarted {
        provider: provider.clone(),
        argv_redacted: argv_redacted.clone(),
        stdin_blob_ref: Some(stdin_blob_ref.clone()),
        model: model_redacted,
        cwd: subprocess_cwd_string.clone(),
        wall_clock_timeout_ms: wall_clock_timeout.as_millis() as u64,
        sandbox_backend: prepared_sandbox.metadata.backend.clone(),
        sandbox_trusted_wrapper: prepared_sandbox.metadata.trusted_wrapper.clone(),
        sandbox_probe_outcome: prepared_sandbox.metadata.probe_outcome.clone(),
        sandbox_write_enforcement: Some(prepared_sandbox.metadata.write_enforcement.clone()),
        sandbox_read_enforcement: Some(prepared_sandbox.metadata.read_enforcement.clone()),
    });

    // ADR-0182: external CLI agents get the same active-task hook binding as
    // direct-agent executions. The AGENT_* fields preserve ORB-10342's
    // commit-telemetry contract and omit unknown model/task values.
    let mut dispatch_env = provenance_env(ProvenanceEnv {
        orbit_run_id: inspection.is_none().then_some(run_id),
        orbit_managed_run_context: true,
        orbit_agent_name: tool_ctx.agent_name.as_deref(),
        orbit_agent_model: tool_ctx.model_name.as_deref(),
        // A source inspection is an Orbit-dispatched invocation without a
        // job run. Give its nested MCP child a separate managed identity.
        orbit_session_id: inspection.as_ref().map(|_| run_id),
        orbit_task_id: task_id,
        orbit_active_task: true,
        agent_run_id: Some(run_id),
        agent_model: model.as_deref(),
        agent_task_id: task_id,
    });
    dispatch_env.push(("ORBIT_TASK_ACTOR_KIND".to_string(), "agent".to_string()));
    // A nested `proc.spawn` may run as long as this invocation has left. The
    // supervisor's clock starts at spawn, a moment after this, so the stamped
    // deadline never outlasts the provider.
    dispatch_env.extend(activity_deadline_env(wall_clock_timeout));
    dispatch_env.extend(activity_policy_env(
        spec,
        activity_name,
        tool_disallow_list.as_deref(),
        &activity_tools.effective_tools,
        fs_profile,
    ));
    dispatch_env.extend(orbit_env);
    // Spawned CLI agents resolve the Orbit registry from $HOME unless a
    // managed registry locator is set. A dispatching run already knows its
    // registry; inject it so a provider whose HOME is a tool-specific
    // directory (e.g. ~/.codex) can still reach `orbit tool run`. [ORB-10909]
    //
    // The injected value is the authoritative shared registry root, never the
    // dispatching checkout's workspace `.orbit`. A managed run's workspace
    // state root is mounted read-only inside the sandbox and does not own the
    // task store. `ORBIT_ROOT` cannot carry this contract because it is the
    // operator's explicit data-root override and deliberately pins global,
    // shared, and local roots together. The managed-only locator changes only
    // the global registry. Workspace selection is the separate logical
    // `ORBIT_WORKSPACE` selector below, not the linked-worktree cwd.
    // [ORB-10980] [ORB-11066] [ORB-11117]
    let registry_locator_injected = if let Some(registry_root) = host.orbit_registry_root() {
        dispatch_env.push((MCP_MANAGED_REGISTRY_ROOT_ENV.to_string(), registry_root));
        true
    } else {
        false
    };
    // Carry the trusted logical `ws_*` identity so nested `orbit tool run`
    // and `orbit mcp serve` do not rediscover ownership from a linked-worktree
    // cwd. The child honors this only with managed provenance plus a run or
    // source-inspection invocation identity;
    // an explicit `--workspace` or tool-payload selector still wins and still
    // fails closed. [ORB-11117]
    if let Some(workspace) = host.orbit_workspace_selector() {
        dispatch_env.push((MCP_MANAGED_WORKSPACE_ENV.to_string(), workspace));
    }
    if let Some(cwd) = subprocess_cwd.as_ref() {
        let scratch = orbit_common::fs::path::ensure_orbit_scratch_dir(cwd).map_err(|error| {
            DispatchError::CliInvocationPermanent(format!(
                "failed to create worker scratch dir: {error}"
            ))
        })?;
        dispatch_env.push((
            orbit_common::fs::path::ORBIT_SCRATCH_DIR_ENV.to_string(),
            scratch.display().to_string(),
        ));
    }
    // The child's whole environment is composed here and applied to a cleared
    // one by every launcher, so the `[execution.env]` allowlist governs what an
    // untrusted provider subprocess can read. The provider's declared
    // `required_env_vars` ride along as extras so a strict allowlist still
    // starts the CLI. `dispatch_env` is appended last and later entries win, so
    // this run's identity and tool pinning override any same-named value the
    // allowlist forwarded from an outer process. [ORB-10917]
    let mut child_env =
        provider_child_environment(host, &provider, sandbox, invocation.required_env_vars);
    if inspection.is_some() {
        // This invocation has no job-run authority. An outer managed process
        // may have passed its own run id through the allowlist.
        child_env.retain(|(key, _)| key != "ORBIT_RUN_ID");
    }
    if registry_locator_injected {
        // A host process may itself have been launched with an operator
        // `ORBIT_ROOT`. Do not reinterpret that pinned-data-root input as the
        // managed child's workspace root; the authoritative registry locator
        // above supersedes it for this execution envelope. [ORB-11066]
        child_env.retain(|(key, _)| key != "ORBIT_ROOT");
    }
    // The envelope allowlist forwards an outer run's activity and process
    // policy names; drop them, then stamp this activity's envelope.
    drop_inherited_policy_env(&mut child_env);
    // Provider-pinned entries replace any same-named value the allowlist
    // forwarded, so an outer process cannot re-enable what the provider
    // disables for headless runs. [ORB-13664]
    child_env.retain(|(key, _)| {
        !invocation
            .fixed_env
            .iter()
            .any(|(fixed, _)| *fixed == key.as_str())
    });
    child_env.extend(
        invocation
            .fixed_env
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string())),
    );
    child_env.extend(dispatch_env);
    if host.worker_invocation().is_some() {
        child_env.push(("ORBIT_WORKER_CONTEXT_REQUIRED".into(), "1".into()));
    }
    // The broker listens before the provider exists and is torn down when it
    // exits. Its path is exported only when the socket bound; an outer value
    // the allowlist forwarded never names this run's broker.
    // Everything the broker authorizes with is this run's own record: its
    // tool policy, identity and the sandbox the agent was given.
    let broker_worktree = subprocess_cwd
        .clone()
        .or_else(|| tool_ctx.workspace_root.clone());
    let broker_run = sandbox
        .zip(broker_worktree)
        .map(|(sandbox, worktree)| PluginBrokerRun {
            run_id: run_id.to_string(),
            job_run_id: inspection.is_none().then(|| run_id.to_string()),
            task_id: task_id.map(str::to_string),
            activity_name: activity_name.to_string(),
            agent_name: tool_ctx.agent_name.clone(),
            model_name: tool_ctx.model_name.clone(),
            workspace: host.orbit_workspace_selector(),
            allowed_tools: match tool_disallow_list {
                Some(_) => Vec::new(),
                None => activity_tools.effective_tools.clone(),
            },
            tool_deny_policy: tool_disallow_list.as_ref().map(|disallow_list| {
                ActivityToolDenyPolicy {
                    activity: activity_name.to_string(),
                    disallow_list: disallow_list.clone(),
                }
            }),
            caller: BrokeredCaller {
                worktree,
                fs_profile: sandbox.fs_profile.clone(),
                proc_allowed_programs: spec.proc_allowed_programs.clone().unwrap_or_default(),
                proc_disallowed_programs: spec.proc_disallowed_programs.clone(),
            },
        });
    let plugin_broker = RunPluginBroker::start(host, run_id, broker_run.as_ref(), sandbox);
    child_env.retain(|(key, _)| key != crate::context::PLUGIN_BROKER_ENV);
    child_env.extend(plugin_broker.env());
    // [ORB-10496] Record the provider child's PID the moment it exists. Emitted
    // through the same writer, so it is persisted (and therefore readable by
    // `orbit run show` / the run-status API) while the invocation is still
    // running — the only in-flight signal for a ship-pipeline agent step.
    let pid_audit = Arc::clone(&audit);
    let pid_provider = provider.clone();
    let on_spawn = move |pid: u32| {
        pid_audit.emit_lossy(V2AuditEventKind::CliInvocationProcess {
            provider: pid_provider.clone(),
            pid,
            pid_start_time: process_start_identity_token(pid),
        });
    };

    // [ORB-13899] Live progress: the newest assistant message and, through
    // the event time, the last moment the child produced output. Silence
    // emits nothing, so a quiet child adds no rows.
    let progress_audit = Arc::clone(&audit);
    let progress_provider = provider.clone();
    let reported_bytes = Cell::new(0usize);
    let report_progress = |progress: &OutputProgress| {
        if progress.observed_bytes == reported_bytes.replace(progress.observed_bytes) {
            return;
        }
        let message = bounded_assistant_message(
            &progress_provider,
            &progress.recent,
            redaction,
            PROGRESS_MESSAGE_LIMIT_BYTES,
        );
        progress_audit.emit_lossy(V2AuditEventKind::CliInvocationActivity {
            provider: progress_provider.clone(),
            observed_bytes: progress.observed_bytes as u64,
            latest_message_truncated: message.as_ref().is_some_and(|message| message.truncated),
            latest_message: message.map(|message| message.text),
        });
    };

    // A descendant the supervisor ended because it stayed stopped: the
    // operator's record of what was killed and why the agent moved on.
    let stopped_audit = Arc::clone(&audit);
    let stopped_provider = provider.clone();
    let report_stopped = |stopped: &StoppedDescendant| {
        tracing::warn!(
            target: "orbit.engine.cli_runner",
            provider = %stopped_provider,
            job_run_id = %run_id,
            pid = stopped.pid,
            ended = stopped.ended(),
            "{}",
            stopped.describe()
        );
        stopped_audit.emit_lossy(V2AuditEventKind::CliInvocationStoppedDescendant {
            provider: stopped_provider.clone(),
            pid: stopped.pid,
            pid_start_time: stopped.pid_start_time.clone(),
            command: stopped.command.clone(),
            stopped_ms: u64::try_from(stopped.stopped_for.as_millis()).unwrap_or(u64::MAX),
            ended: stopped.ended(),
            error: stopped.end_error.clone(),
        });
    };

    // A managed Linux Bubblewrap launch snapshots the write-policy gaps its
    // mounts cannot cover while compiling those mounts; take that snapshot off
    // the spawned child rather than walking the worktree a second time.
    let mut linux_post_run_guard = None;
    let spawn_result = spawn_for_supervision(
        &resolved_program,
        &subprocess_args,
        &child_env,
        subprocess_cwd.as_deref(),
        sandbox,
        &provider,
    )
    .and_then(|mut spawned| {
        let registered = host
            .register_worker_process(spawned.child.id())
            .and_then(|()| {
                if sandbox.is_some_and(|sandbox| {
                    sandbox.kind == orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap
                }) {
                    host.register_worker_pid_namespace(spawned.child.id())
                } else {
                    Ok(())
                }
            });
        if let Err(error) = registered {
            let _ = spawned.child.kill();
            let _ = spawned.child.wait();
            return Err(SpawnError::permanent(error.to_string()));
        }
        plugin_broker.bind_sandbox(spawned.child.id());
        linux_post_run_guard = spawned.take_linux_post_run_guard();
        spawn_with_timeout(SpawnWithTimeoutRequest {
            program: &resolved_program,
            args: &subprocess_args,
            stdin_bytes: &invocation.stdin,
            env: &child_env,
            cwd: subprocess_cwd.as_deref(),
            timeout: wall_clock_timeout,
            sandbox,
            trace: SpawnTraceContext {
                provider: &provider,
                job_run_id: run_id,
                task_id: task_id_from_input(input),
                cwd: subprocess_cwd_string.as_deref(),
            },
            output_capture_limit: None,
            on_spawn: Some(&on_spawn),
            on_progress: Some(ProgressReporter {
                interval: PROVIDER_PROGRESS_INTERVAL,
                report: &report_progress,
            }),
            stopped_descendants: Some(StoppedDescendantReporter {
                threshold: stopped_descendant_threshold(),
                report: &report_stopped,
            }),
            wait: None,
            live_readers: None,
            spawned_child: Some(spawned),
            #[cfg(unix)]
            cancel_pair: None,
        })
    });
    // The provider has exited, timed out, or failed to start: nothing in its
    // sandbox may reach the broker any longer.
    drop(plugin_broker);

    let (stdout, stderr, exit_code, duration, timed_out) = match spawn_result {
        Ok(result) => result,
        Err(err) => {
            if let Some(boundary) = worktree_boundary.take() {
                boundary.verify_after_provider(
                    host,
                    false,
                    input.get("failed_step_id").and_then(Value::as_str),
                    &task_ids,
                )?;
            }
            // Spawn-layer classification (ORB-10006): executable missing /
            // permission denied fail fast; resource exhaustion (EAGAIN,
            // ENOMEM, ...) and other transient host failures stay retryable
            // at the step layer.
            return Err(if err.permanent {
                DispatchError::CliInvocationPermanent(err.message)
            } else {
                DispatchError::CliInvocationFailed(err.message)
            });
        }
    };

    // A post-run check fails the step, but the run trail must still hold the
    // provider's output. Refresh before those writes: a sandboxed provider may
    // have opened the granted SQLite/WAL files while this worker kept its
    // pre-spawn handles, and every clone of that store shares the connection.
    // The swap happens only after the replacement is open and writable, so a
    // failure before it leaves the previous connection and a failure after it
    // leaves the new one. Blobs and `CliInvocationFinished` are recorded on
    // whichever connection remains, then inspection, the Linux guard, and the
    // worktree boundary run. Integrity failures keep that classification.
    let refresh_error = host
        .refresh_persistence_after_cli_provider()
        .err()
        .map(|error| {
            DispatchError::CliInvocationPermanent(format!(
                "refresh durable store after provider `{provider}` exited: {error}"
            ))
        });

    let stdout_blob_ref = audit.write_blob(stdout.bytes());
    let stderr_blob_ref = audit.write_blob(stderr.bytes());
    audit.emit_lossy(V2AuditEventKind::CliInvocationFinished {
        provider: provider.clone(),
        exit_code,
        duration_ms: duration.as_millis() as u64,
        stdout_blob_ref: Some(stdout_blob_ref.clone()),
        stderr_blob_ref: Some(stderr_blob_ref.clone()),
        harness_version: None,
        timed_out,
    });

    let inspection_error = inspection
        .as_ref()
        .and_then(|snapshot| snapshot.verify().err());
    let guard_error = linux_post_run_guard.as_ref().and_then(|guard| {
        guard
            .verify()
            .err()
            .map(|error| DispatchError::CliInvocationPermanent(error.to_string()))
    });
    let post_run_error = inspection_error.or(guard_error).or(refresh_error);

    let completion = project_completion(ProviderExit {
        host,
        spec,
        input,
        provider,
        model,
        task_ids: &task_ids,
        worktree_boundary,
        sandbox,
        subprocess_cwd: subprocess_cwd.as_deref(),
        redaction,
        timeout_seconds,
        argv_redacted,
        stdin_blob_ref,
        stdout_blob_ref: stdout_blob_ref.clone(),
        stderr_blob_ref: stderr_blob_ref.clone(),
        stdout,
        stderr,
        exit_code,
        duration,
        timed_out,
        print_timeout,
    });
    step_error_after_provider_evidence(
        completion,
        post_run_error,
        &stdout_blob_ref,
        &stderr_blob_ref,
    )
}

/// Keep a worktree-integrity failure as the step error, and cite the stored
/// provider output on every failure that follows a post-run check.
fn step_error_after_provider_evidence(
    completion: Result<DispatchOutcome, DispatchError>,
    post_run_error: Option<DispatchError>,
    stdout_blob_ref: &str,
    stderr_blob_ref: &str,
) -> Result<DispatchOutcome, DispatchError> {
    let Some(post_run_error) = post_run_error else {
        return completion;
    };
    let cited_post_run = cite_output_evidence(post_run_error, stdout_blob_ref, stderr_blob_ref);
    let error = match completion {
        Err(DispatchError::WorktreeIntegrity { code, diagnostic }) => {
            DispatchError::WorktreeIntegrity {
                code,
                diagnostic: cite_integrity_diagnostic(
                    &diagnostic,
                    stdout_blob_ref,
                    stderr_blob_ref,
                ),
            }
        }
        Err(error) => cite_output_evidence(error, stdout_blob_ref, stderr_blob_ref),
        Ok(_) => cited_post_run,
    };
    Err(error)
}

/// Attach blob refs when `error` carries a message this layer can extend.
/// Other variants fall back to a permanent error that still names the blobs,
/// so a post-run failure cannot return without those references.
fn cite_output_evidence(
    error: DispatchError,
    stdout_blob_ref: &str,
    stderr_blob_ref: &str,
) -> DispatchError {
    let cite = output_blob_cite(stdout_blob_ref, stderr_blob_ref);
    match error {
        DispatchError::CliInvocationFailed(message) => {
            DispatchError::CliInvocationFailed(format!("{message} ({cite})"))
        }
        DispatchError::CliInvocationPermanent(message) => {
            DispatchError::CliInvocationPermanent(format!("{message} ({cite})"))
        }
        DispatchError::JobExecution(message) => {
            DispatchError::JobExecution(format!("{message} ({cite})"))
        }
        DispatchError::GitTimeout {
            operation,
            root,
            timeout_ms,
            diagnostic,
        } => DispatchError::GitTimeout {
            operation,
            root,
            timeout_ms,
            diagnostic: format!("{diagnostic} ({cite})"),
        },
        DispatchError::WorktreeIntegrity { code, diagnostic } => DispatchError::WorktreeIntegrity {
            code,
            diagnostic: cite_integrity_diagnostic(&diagnostic, stdout_blob_ref, stderr_blob_ref),
        },
        other => DispatchError::CliInvocationPermanent(format!("{other} ({cite})")),
    }
}

fn cite_integrity_diagnostic(
    diagnostic: &str,
    stdout_blob_ref: &str,
    stderr_blob_ref: &str,
) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(diagnostic) else {
        return format!(
            "{diagnostic} ({})",
            output_blob_cite(stdout_blob_ref, stderr_blob_ref)
        );
    };
    let Some(object) = value.as_object_mut() else {
        return format!(
            "{diagnostic} ({})",
            output_blob_cite(stdout_blob_ref, stderr_blob_ref)
        );
    };
    object.insert(
        "stdout_blob_ref".to_string(),
        Value::String(stdout_blob_ref.to_string()),
    );
    object.insert(
        "stderr_blob_ref".to_string(),
        Value::String(stderr_blob_ref.to_string()),
    );
    value.to_string()
}

fn output_blob_cite(stdout_blob_ref: &str, stderr_blob_ref: &str) -> String {
    format!("stdout_blob_ref={stdout_blob_ref}, stderr_blob_ref={stderr_blob_ref}")
}

/// Compose the provider environment while admitting Codex's two documented
/// CA-bundle overrides only when Orbit's macOS wrapper needs them.
///
/// The variables remain outside the general agent baseline: other providers,
/// bare Codex invocations, and Linux keep their existing environment surface.
/// The macOS spawn layer supplies a public system bundle only when neither
/// explicit value is present.
pub(crate) fn provider_child_environment(
    host: &dyn RuntimeHost,
    provider: &str,
    sandbox: Option<&super::super::super::dispatcher::ResolvedSandbox>,
    required_env_vars: &[&str],
) -> Vec<(String, String)> {
    let needs_codex_ca_overrides = provider == "codex"
        && sandbox.is_some_and(|sandbox| sandbox.kind == ExecutorSandboxKind::MacosSandboxExec);
    if !needs_codex_ca_overrides {
        return host.agent_subprocess_environment(required_env_vars);
    }

    let mut env_names = required_env_vars.to_vec();
    env_names.extend([CODEX_CA_CERTIFICATE_ENV, SSL_CERT_FILE_ENV]);
    host.agent_subprocess_environment(&env_names)
}
