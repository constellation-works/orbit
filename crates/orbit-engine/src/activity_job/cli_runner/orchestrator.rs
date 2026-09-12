//! Orchestration for `backend: cli` agent subprocess dispatch.

use std::sync::Arc;
use std::time::Duration;

use orbit_agent::{
    Agent, AgentConfig, AgentOperation, AgentRequest, antigravity_terminal_error_diagnostic,
    normalize_cli_stdout, peek_declared_response_failure, peek_response_status,
    project_cli_response, provider_invocation_diagnostic, response_envelope_protocol_check,
};
use orbit_common::process::identity::process_start_identity_token;
use orbit_common::security::redaction::{PatternRedactor, redact_sensitive_env_text};
use orbit_types::policy::UNRESTRICTED_FS_PROFILE;
use orbit_types::workflow::ExecutorSandboxKind;
use orbit_types::workflow::activity_job::{AgentLoopSpec, TrustedHostAdmission, V2AuditEventKind};
use serde_json::Value;

use crate::context::{ProvenanceEnv, provenance_env};

use super::super::audit_writer::V2AuditWriter;
use super::super::dispatcher::{DispatchError, DispatchInvocationTrace, DispatchOutcome};
use super::super::workspace::{
    WorktreeBoundaryGuard, resolve_subprocess_cwd, validate_declared_worktree_pair,
};
use super::argv::{
    apply_provider_runtime_arg_fixups, apply_provider_static_arg_fixups,
    apply_trusted_host_provider_sandbox, neutralize_inner_sandbox, try_audit_argv_for_dispatch,
};
use super::envelope::{
    cli_agent_envelope_json, parse_cli_invocation_trace, parse_cli_response_result,
    task_id_from_input, task_ids_from_input,
};
use super::inspection::SourceInspection;
use super::spawn::{
    CODEX_CA_CERTIFICATE_ENV, PreparedSandbox, SSL_CERT_FILE_ENV,
    copilot_model_unavailable_diagnostic, linux_bwrap_failed_write_diagnostic,
    macos_keychain_auth_diagnostic, orbit_tool_env, prepare_sandbox_for_dispatch,
    resolve_provider_launcher,
};
use super::supervisor::{
    DEFAULT_WALL_CLOCK_TIMEOUT_SECONDS, SpawnTraceContext, SpawnWithTimeoutRequest,
    spawn_with_timeout,
};
use crate::context::RuntimeHost;
use orbit_exec::LinuxBwrapPostRunGuard;

const STDOUT_TEXT_PREVIEW_LIMIT_BYTES: usize = 64 * 1024;
const RESPONSE_DIAGNOSTIC_LIMIT_CHARS: usize = 1024;

pub fn run_cli_backend(
    host: &dyn RuntimeHost,
    spec: &AgentLoopSpec,
    activity_name: &str,
    run_id: &str,
    audit: Arc<V2AuditWriter>,
    input: &Value,
    fs_profile: Option<&str>,
) -> Result<DispatchOutcome, DispatchError> {
    let provider = spec.provider.as_str().to_string();
    // [ORB-11354] Trusted host execution needs both halves: the built-in
    // activity declares the mode, and the operator's canonical submission
    // stamps the admission into the run input. A declaration without an
    // admission is a broken admission path, not a request for a sandboxed run,
    // so it fails closed here rather than silently downgrading.
    let trusted_host = if spec.trusted_host_execution {
        Some(TrustedHostAdmission::from_run_input(input).ok_or_else(|| {
            DispatchError::CliInvocationPermanent(format!(
                "activity `{activity_name}` declares trusted host execution but this run carries \
                 no operator admission; submit it through the governed `orbit.agent.invoke` \
                 operation"
            ))
        })?)
    } else {
        None
    };
    let mut cli_executor = host.resolve_cli_executor(&provider)?;
    let declared_timeout_seconds = if spec.wall_clock_timeout_seconds == 0 {
        DEFAULT_WALL_CLOCK_TIMEOUT_SECONDS
    } else {
        spec.wall_clock_timeout_seconds
    };
    // [ORB-11354] An operator submitting an exploration says how long they are
    // willing to wait. The request can only *shorten* the activity's declared
    // bound, so the asset stays the ceiling and no run input can extend an
    // unsandboxed subprocess past it. Only the admitted mode reads the key;
    // every other activity keeps its declared timeout verbatim.
    let timeout_seconds = match trusted_host.as_ref().and(
        input
            .get("timeout_seconds")
            .and_then(Value::as_u64)
            .filter(|seconds| *seconds > 0),
    ) {
        Some(requested) => requested.min(declared_timeout_seconds),
        None => declared_timeout_seconds,
    };
    // A reviewer invocation may carry the captured leftover lineage
    // allowance. It can only shorten the same ceiling; a zero leftover
    // still gets one second so the process is not unbounded.
    let timeout_seconds = match input.get("remaining_seconds").and_then(Value::as_u64) {
        Some(remaining) => timeout_seconds.min(remaining.max(1)),
        None => timeout_seconds,
    };
    let wall_clock_timeout = Duration::from_secs(timeout_seconds);

    let task_ids = task_ids_from_input(input);
    let task_id = task_id_from_input(input);
    if activity_name == "pr_conflict_recovery"
        && (task_ids.is_empty() || input.get("run_id").and_then(Value::as_str) != Some(run_id))
    {
        return Err(DispatchError::CliInvocationPermanent(
            "conflict recovery requires task IDs and the current run ID from its failed target"
                .to_string(),
        ));
    }
    let activity_tools = host.resolve_activity_tools(&task_ids, &spec.tools)?;

    // §6 allowlist-advisory event — emitted once per invocation before the
    // subprocess starts so a reviewer can see the enforcement gap at a glance.
    audit.emit_lossy(V2AuditEventKind::ToolAllowlistHarnessDelegated {
        provider: provider.clone(),
        task_id: task_id.map(ToOwned::to_owned),
        task_ids: task_ids.clone(),
        requested_tools: activity_tools.requested_tools.clone(),
        effective_tools: activity_tools.effective_tools.clone(),
        tools: activity_tools.effective_tools.clone(),
    });

    let task_ctx = host.task_context_for_agent_input(input)?;
    let mut tool_ctx = host.tool_context_for_activity(
        Some(run_id),
        fs_profile,
        None,
        spec.proc_allowed_programs.as_deref(),
    );
    tool_ctx.agent_name = Some(provider.clone());
    tool_ctx.model_name = spec.model.as_deref().map(str::to_string);
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
    // Resolve the subprocess cwd before sandbox compilation so the host can
    // re-allow the active worktree subpath after the policy deny rules. The
    // sandbox's `denyModify .orbit/**` rule otherwise blocks every non-codex
    // provider from writing inside its own jrun worktree. See T20260508-17.
    let source_cwd =
        resolve_subprocess_cwd(input, task_ctx.as_ref(), tool_ctx.workspace_root.as_deref())?;
    let inspection = SourceInspection::from_input(input, source_cwd.as_deref(), fs_profile)?;
    let inspection_input = inspection
        .as_ref()
        .map(|snapshot| snapshot.bind_input(input));
    let inspection_task_ctx = inspection
        .as_ref()
        .and_then(|snapshot| task_ctx.as_ref().map(|task| snapshot.bind_input(task)));
    let subprocess_cwd = inspection
        .as_ref()
        .map(|snapshot| snapshot.root().to_path_buf())
        .or_else(|| source_cwd.clone());
    let subprocess_cwd_string = subprocess_cwd
        .as_ref()
        .map(|path| path.display().to_string());
    // An admitted trusted-host invocation skips sandbox resolution entirely
    // rather than resolving one and discarding it: there is no profile to
    // compile, no wrapper to probe, and no inner-sandbox flag to neutralize.
    // Every other activity, including every managed task job, is unchanged.
    let resolved_sandbox = match &trusted_host {
        Some(_) => None,
        None => host.resolve_executor_sandbox(&provider, fs_profile, subprocess_cwd.as_deref())?,
    };
    let prepared_sandbox = match &trusted_host {
        Some(_) => PreparedSandbox::none_trusted_host(),
        None => prepare_sandbox_for_dispatch(resolved_sandbox.as_ref())
            .map_err(|error| DispatchError::CliInvocationPermanent(error.message))?,
    };
    let sandbox = prepared_sandbox.effective;

    let envelope_json = cli_agent_envelope_json(
        spec,
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

    let mut subprocess_args = Vec::with_capacity(cli_executor.args.len() + invocation.args.len());
    subprocess_args.extend(cli_executor.args.iter().cloned());
    subprocess_args.extend(invocation.args.iter().cloned());
    // Combined executor + transport argv is the only place that can honor a
    // custom `--print-timeout` without duplicating it, and the remaining
    // spawn deadline is known here. [ORB-11337]
    apply_provider_runtime_arg_fixups(&provider, &mut subprocess_args, wall_clock_timeout);

    // The audit argv reflects what actually runs. Under sandbox-exec the
    // parent is `<trusted sandbox-exec> -f <profile.sb> <program> <args...>`;
    // under bare exec it's `<program> <args...>`. The redactor still scrubs
    // the child's program name + args so secrets in argv stay redacted.
    let redaction = PatternRedactor::with_argv_secrets();
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
    )?;

    if activity_name == "pr_conflict_recovery" {
        let boundary = worktree_boundary.as_mut().ok_or_else(|| {
            DispatchError::CliInvocationPermanent(
                "conflict recovery requires a validated assigned worktree; refusing primary-checkout execution".to_string(),
            )
        })?;
        boundary.authorize_rebase_completion(input)?;
    }

    if let Some(admission) = &trusted_host {
        tracing::warn!(
            target: "orbit.trusted_host",
            run_id,
            activity_name,
            provider = %provider,
            authorized_by = %admission.authorized_by,
            authorizer_provenance = %admission.authorizer_provenance,
            caller_machine_id = admission.caller_machine_id.as_deref(),
            caller_identity = admission.caller_identity.map(|identity| identity.to_string()),
            agent_invoke_mode = admission.agent_invoke_mode.map(|mode| mode.to_string()),
            workspace_path = %admission.workspace_path,
            cwd = %admission.cwd,
            "starting an operator-admitted provider subprocess outside the executor sandbox"
        );
        audit.emit_lossy(V2AuditEventKind::TrustedHostExecutionAdmitted {
            provider: provider.clone(),
            activity_name: activity_name.to_string(),
            authorized_by: admission.authorized_by.clone(),
            authorizer_provenance: admission.authorizer_provenance.clone(),
            caller_machine_id: admission.caller_machine_id.clone(),
            caller_identity: admission.caller_identity,
            agent_invoke_mode: admission.agent_invoke_mode,
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
        orbit_run_id: Some(run_id),
        orbit_managed_run_context: true,
        orbit_agent_name: tool_ctx.agent_name.as_deref(),
        orbit_agent_model: tool_ctx.model_name.as_deref(),
        orbit_session_id: None,
        orbit_task_id: task_id,
        orbit_active_task: true,
        agent_run_id: Some(run_id),
        agent_model: model.as_deref(),
        agent_task_id: task_id,
    });
    dispatch_env.push(("ORBIT_TASK_ACTOR_KIND".to_string(), "agent".to_string()));
    dispatch_env.push((
        "ORBIT_ACTIVITY_TOOLS".to_string(),
        activity_tools.effective_tools.join(","),
    ));
    if let Some(programs) = spec.proc_allowed_programs.as_deref() {
        dispatch_env.push((
            "ORBIT_PROC_ALLOWED_PROGRAMS".to_string(),
            programs.join(","),
        ));
    }
    dispatch_env.push((
        "ORBIT_ACTIVITY_FS_PROFILE".to_string(),
        resolved_activity_fs_profile_name(fs_profile).to_string(),
    ));
    dispatch_env.extend(
        orbit_tool_env().map_err(|error| DispatchError::CliInvocationPermanent(error.message))?,
    );
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
        dispatch_env.push(("ORBIT_REGISTRY_ROOT".to_string(), registry_root));
        true
    } else {
        false
    };
    // Carry the trusted logical `ws_*` identity so nested `orbit tool run`
    // and `orbit mcp serve` do not rediscover ownership from a linked-worktree
    // cwd. The child honors this only together with managed-run provenance;
    // an explicit `--workspace` or tool-payload selector still wins and still
    // fails closed. [ORB-11117]
    if let Some(workspace) = host.orbit_workspace_selector() {
        dispatch_env.push(("ORBIT_WORKSPACE".to_string(), workspace));
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
    if registry_locator_injected {
        // A host process may itself have been launched with an operator
        // `ORBIT_ROOT`. Do not reinterpret that pinned-data-root input as the
        // managed child's workspace root; the authoritative registry locator
        // above supersedes it for this execution envelope. [ORB-11066]
        child_env.retain(|(key, _)| key != "ORBIT_ROOT");
    }
    child_env.extend(dispatch_env);
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

    let linux_post_run_guard = match sandbox {
        Some(sandbox)
            if sandbox.kind == orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap
                && sandbox.managed_worktree =>
        {
            LinuxBwrapPostRunGuard::capture(&sandbox.fs_profile)
                .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))?
        }
        _ => None,
    };

    let spawn_result = spawn_with_timeout(SpawnWithTimeoutRequest {
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
        wait: None,
        live_readers: None,
        #[cfg(unix)]
        cancel_pair: None,
    });

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

    if let Some(snapshot) = &inspection {
        snapshot.verify()?;
    }

    if let Some(guard) = linux_post_run_guard {
        guard
            .verify()
            .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))?;
    }

    let stdout_blob_ref = audit.write_blob(stdout.bytes());
    let stderr_blob_ref = audit.write_blob(stderr.bytes());

    // The provider has exited and the supervisor owns its exact status,
    // captured response, and durable blob evidence. Rebind before the first
    // completion event: a sandboxed provider may have opened the explicitly
    // granted SQLite/WAL files while this long-lived worker retained handles
    // from before spawn.
    host.refresh_persistence_after_cli_provider()
        .map_err(|error| {
            DispatchError::CliInvocationPermanent(format!(
                "refresh durable store after provider `{provider}` exited: {error}"
            ))
        })?;

    audit.emit_lossy(V2AuditEventKind::CliInvocationFinished {
        provider: provider.clone(),
        exit_code,
        duration_ms: duration.as_millis() as u64,
        stdout_blob_ref: Some(stdout_blob_ref.clone()),
        stderr_blob_ref: Some(stderr_blob_ref.clone()),
        harness_version: None,
        timed_out,
    });

    // Provider output is not the system of record for artifact-backed
    // activities: task state, review threads, git state, and deterministic
    // downstream gates are. Parse response envelopes to project useful fields
    // and diagnostics, but only make them authoritative when the activity
    // explicitly declares that downstream templates require them.
    let exit_success = !timed_out && matches!(exit_code, Some(0));
    // Attribution is deliberately not gated on the exit code. An agent that
    // hits a policy-denied write mid-turn can still exit 0 — it narrates the
    // denial and stops without emitting a terminating envelope — and that
    // exit-0 shape is precisely the one that reached an operator with no path
    // and no rule. Deriving the diagnostic here costs a substring scan of
    // stderr (the helper skips any line without an EROFS marker) and keeps the
    // Orbit-owned attribution available to every failure branch below.
    let sandbox_write_diagnostic = match sandbox {
        Some(sandbox) if sandbox.kind == orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap => {
            linux_bwrap_failed_write_diagnostic(
                &sandbox.fs_profile,
                stderr.protocol_bytes(),
                subprocess_cwd.as_deref(),
            )
            .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))?
            .map(|diagnostic| bounded_diagnostic(&diagnostic, &redaction))
        }
        _ => None,
    };
    // A truncated capture retains the final complete JSONL events separately
    // from its diagnostic prefix. Protocol parsing must use that tail so a
    // verbose provider's final Orbit envelope remains authoritative.
    //
    // Keep invocation telemetry distinct from answer projection. Provider
    // JSONL carries usage, tool traffic, failures, reasoning, and command
    // output; those frames belong in the trace and diagnostics, but only
    // provider-attributed assistant answer content may supply Orbit response
    // fields or completion status. [ORB-10946] [ORB-11348]
    let trace_stdout = normalize_cli_stdout(&provider, stdout.protocol_bytes());
    let answer_stdout = project_cli_response(&provider, stdout.protocol_bytes());
    let answer_text = String::from_utf8_lossy(answer_stdout.as_ref());
    let trace_stdout_text = String::from_utf8_lossy(trace_stdout.as_ref());
    let declared_failure = peek_declared_response_failure(answer_text.as_ref());
    let envelope_status = declared_failure
        .as_ref()
        .map(|failure| failure.status.clone())
        .or_else(|| peek_response_status(answer_text.as_ref()));
    // The operator-facing preview stays on the *raw* capture: normalization
    // drops the session control plane, and that is where a provider puts the
    // policy and authentication failures an operator needs to see.
    let raw_stdout_text = String::from_utf8_lossy(stdout.protocol_bytes());
    let stdout_preview =
        stdout_text_preview(raw_stdout_text.as_ref(), &redaction, stdout.truncated());
    let parsed_result = exit_success.then(|| {
        parse_cli_response_result(
            answer_stdout.as_ref(),
            stderr.protocol_bytes(),
            exit_code,
            duration.as_millis() as u64,
            true,
        )
    });
    let response_envelope_valid = matches!(parsed_result.as_ref(), Some(Ok(_)));
    let response_envelope_error = parsed_result
        .as_ref()
        .and_then(|result| result.as_ref().err())
        .map(|error| response_diagnostic(error, &redaction));
    // [ORB-10449]: the step-completion protocol check. Content-blind
    // by construction — `response_envelope_protocol_check` reads the envelope
    // frame and never `result`/`error`, so this asks only "did the invocation
    // run its contract to the end", never "do we believe what it said". An
    // agent that declares `status: "failed"` passes this frame check; one that
    // yielded mid-turn emitted no envelope and does not.
    //
    // Only meaningful on an otherwise-clean exit: a timeout or nonzero exit
    // already fails the step with a more specific message.
    let completion_envelope_error = exit_success
        .then(|| response_envelope_protocol_check(answer_text.as_ref()))
        .and_then(Result::err)
        .map(|error| completion_diagnostic(&error.to_string(), &redaction));
    let completion_protocol_violation =
        spec.require_completion_envelope && completion_envelope_error.is_some();
    // [ORB-10733] Protocol termination and control-plane outcome are distinct:
    // all recognized status tokens finish the frame, but a required completion
    // contract cannot checkpoint an explicit failed/timeout outcome. Only the
    // token controls this; the envelope's result/error payload stays advisory.
    let completion_status_failure = spec.require_completion_envelope
        && completion_envelope_error.is_none()
        && matches!(envelope_status.as_deref(), Some("failed") | Some("timeout"));
    // Two orthogonal contracts. `require_completion_envelope` gates step
    // completion and its status outcome (above); `require_response_envelope` additionally gates the
    // envelope's *content* for activities whose downstream templates consume it
    // (ADR-0224 / L-0087) — outside that opt-in, parsing stays advisory.
    let success = exit_success
        && !completion_protocol_violation
        && !completion_status_failure
        && (!spec.require_response_envelope || response_envelope_valid);

    // A conflict-recovery provider repairs files only. Once its process has
    // satisfied the activity completion contract, the host-side boundary
    // independently revalidates live ownership, stages exactly the conflict
    // set, and continues the checkpointed rebase. No response result field is
    // consulted. Ordinary providers retain the same post-run integrity check.
    if let Some(boundary) = worktree_boundary {
        boundary.verify_after_provider(
            host,
            success,
            input.get("failed_step_id").and_then(Value::as_str),
            &task_ids,
        )?;
    }
    let trace = parse_cli_invocation_trace(
        trace_stdout.as_ref(),
        stderr.protocol_bytes(),
        exit_code,
        duration.as_millis() as u64,
        success,
    );
    let message = if timed_out {
        Some(format!(
            "cli subprocess exceeded {}s wall-clock timeout",
            timeout_seconds
        ))
    } else if !exit_success {
        let stderr_text = String::from_utf8_lossy(stderr.protocol_bytes());
        let exit_message = || format!("cli subprocess exited with code {exit_code:?}");
        Some(
            sandbox_write_diagnostic
                .clone()
                // Copilot reports an unavailable explicit model only on
                // stderr. Join it to the resolved crew before the generic
                // exit-code path loses the configuration source.
                .or_else(|| {
                    input
                        .get("crew")
                        .and_then(Value::as_str)
                        .and_then(|crew| {
                            copilot_model_unavailable_diagnostic(
                                &provider,
                                crew,
                                stderr_text.as_ref(),
                            )
                        })
                        .map(|diagnostic| {
                            format!(
                                "{} {}",
                                exit_message(),
                                bounded_diagnostic(&diagnostic, &redaction)
                            )
                        })
                })
                // A Keychain-backed provider login reads as "expired" whether it
                // really expired or the sandbox hid the credential. Orbit
                // compiled the profile, so it is the layer that can say which
                // one this was — and the provider's own message cannot.
                // [ORB-10929]
                .or_else(|| {
                    macos_keychain_auth_diagnostic(
                        &provider,
                        sandbox,
                        &format!("{trace_stdout_text}\n{stderr_text}"),
                    )
                    .map(|diagnostic| format!("{} {diagnostic}", exit_message()))
                })
                // [ORB-10746] A bare exit code cannot distinguish "this CLI
                // has no --json-schema" from "the provider rejected Orbit's
                // schema" from any other nonzero exit, and the first two are
                // configuration faults an operator can act on immediately.
                .or_else(|| {
                    provider_invocation_diagnostic(trace_stdout_text.as_ref(), stderr_text.as_ref())
                        .map(|diagnostic| bounded_diagnostic(&diagnostic, &redaction))
                })
                // Antigravity writes terminal `ERROR` on stdout and often
                // leaves stderr empty. Read the raw capture: normalization
                // drops failed terminals so they cannot satisfy completion.
                // [ORB-11337]
                .or_else(|| {
                    antigravity_terminal_error_diagnostic(&provider, stdout.protocol_bytes()).map(
                        |diagnostic| {
                            format!(
                                "{} {}",
                                exit_message(),
                                bounded_diagnostic(&diagnostic, &redaction)
                            )
                        },
                    )
                })
                .unwrap_or_else(exit_message),
        )
    } else if (spec.require_completion_envelope || spec.require_response_envelope)
        && matches!(envelope_status.as_deref(), Some("failed") | Some("timeout"))
    {
        Some(with_sandbox_write_attribution(
            declared_failure_diagnostic(
                envelope_status.as_deref().unwrap_or("unknown"),
                declared_failure.as_ref(),
                &redaction,
            ),
            sandbox_write_diagnostic.as_deref(),
        ))
    } else if spec.require_response_envelope {
        response_envelope_error
            .clone()
            .map(|error| with_sandbox_write_attribution(error, sandbox_write_diagnostic.as_deref()))
    } else if completion_protocol_violation {
        // Ordered last on purpose: an activity that opted into the content
        // contract already produced a strictly more specific diagnostic above,
        // and the two conditions largely overlap. This branch is what the
        // remaining activities — the ones that only ever had the advisory
        // parse — now report instead of silently checkpointing success.
        completion_envelope_error
            .clone()
            .map(|error| with_sandbox_write_attribution(error, sandbox_write_diagnostic.as_deref()))
    } else {
        None
    };

    let StdoutTextPreview {
        text: stdout_text,
        truncated: stdout_text_truncated,
        preview_bytes: stdout_text_preview_bytes,
    } = stdout_preview;
    let mut output = parsed_result.and_then(Result::ok).unwrap_or_default();
    for (key, value) in [
        ("provider", Value::String(provider.clone())),
        ("argv_redacted", serde_json::json!(argv_redacted)),
        ("stdin_blob_ref", Value::String(stdin_blob_ref.clone())),
        ("stdout_blob_ref", Value::String(stdout_blob_ref.clone())),
        ("stderr_blob_ref", Value::String(stderr_blob_ref.clone())),
        ("exit_code", serde_json::json!(exit_code)),
        (
            "duration_ms",
            serde_json::json!(duration.as_millis() as u64),
        ),
        ("timed_out", Value::Bool(timed_out)),
        (
            "response_envelope_required",
            Value::Bool(spec.require_response_envelope),
        ),
        (
            "response_envelope_valid",
            Value::Bool(response_envelope_valid),
        ),
        (
            "response_envelope_status",
            envelope_status.map_or(Value::Null, Value::String),
        ),
        (
            "response_envelope_error",
            response_envelope_error.map_or(Value::Null, Value::String),
        ),
        (
            "completion_envelope_required",
            Value::Bool(spec.require_completion_envelope),
        ),
        (
            "completion_envelope_satisfied",
            Value::Bool(!exit_success || completion_envelope_error.is_none()),
        ),
        (
            "completion_envelope_error",
            completion_envelope_error.map_or(Value::Null, Value::String),
        ),
        (
            "sandbox_write_diagnostic",
            sandbox_write_diagnostic.map_or(Value::Null, Value::String),
        ),
        ("stdout_text", Value::String(stdout_text)),
        (
            "stdout_text_truncated",
            Value::Bool(stdout.truncated() || stdout_text_truncated),
        ),
        (
            "stdout_text_original_bytes",
            serde_json::json!(stdout.observed_bytes()),
        ),
        (
            "stdout_text_preview_bytes",
            serde_json::json!(stdout_text_preview_bytes),
        ),
        (
            "stdout_text_preview_limit_bytes",
            serde_json::json!(STDOUT_TEXT_PREVIEW_LIMIT_BYTES),
        ),
        (
            "stdout_text_captured_bytes",
            serde_json::json!(stdout.bytes().len()),
        ),
        ("stdout_capture_truncated", Value::Bool(stdout.truncated())),
        (
            "stdout_capture_limit_bytes",
            serde_json::json!(stdout.capture_limit_bytes()),
        ),
        (
            "stderr_original_bytes",
            serde_json::json!(stderr.observed_bytes()),
        ),
        (
            "stderr_captured_bytes",
            serde_json::json!(stderr.bytes().len()),
        ),
        ("stderr_capture_truncated", Value::Bool(stderr.truncated())),
        (
            "stderr_capture_limit_bytes",
            serde_json::json!(stderr.capture_limit_bytes()),
        ),
    ] {
        output.entry(key.to_string()).or_insert(value);
    }

    Ok(DispatchOutcome {
        success,
        output: Value::Object(output),
        message,
        invocation: trace.map(|trace| DispatchInvocationTrace {
            provider,
            model,
            trace,
        }),
    })
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
    sandbox: Option<&super::super::dispatcher::ResolvedSandbox>,
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

pub(super) fn resolved_activity_fs_profile_name(fs_profile: Option<&str>) -> &str {
    fs_profile.unwrap_or(UNRESTRICTED_FS_PROFILE)
}

/// Append the Orbit-owned write-denial attribution to a step message that was
/// classified from the protocol frame alone.
///
/// A provider that exits 0 after a policy-denied write yields a frame-shaped
/// message ("no terminating envelope") that says nothing about *why* the agent
/// stopped. The sandbox diagnostic is the only text in the system that names
/// the attempted path and the rule that shadowed it, so it rides along rather
/// than replacing the frame classification — the step still failed for the
/// protocol reason, and the denial is the cause worth acting on.
///
/// `diagnostic` is already the `linux_bwrap_write_grant_diagnostic` string
/// (bounded and redacted); this deliberately does not reformat it, so operators
/// and greps see one message format for a write denial regardless of which
/// branch surfaced it.
// pub(super) widened for tests/ layout under ORB-00225; test reaches via exposed surface.
pub(super) fn with_sandbox_write_attribution(message: String, diagnostic: Option<&str>) -> String {
    match diagnostic {
        Some(diagnostic) => format!("{message} {diagnostic}"),
        None => message,
    }
}

fn response_diagnostic(error: &str, redactor: &PatternRedactor) -> String {
    format!(
        "cli response envelope invalid: {}",
        bounded_diagnostic(error, redactor)
    )
}

/// [ORB-10449] Name the protocol violation for what it is. The old surfaced
/// failure was whatever deterministic gate tripped several steps later, which
/// reads as a downstream defect; this says the agent stopped before finishing
/// its turn and points at the evidence.
fn completion_diagnostic(error: &str, redactor: &PatternRedactor) -> String {
    format!(
        "agent step did not complete: the provider exited 0 but stdout carried no valid \
         terminating Orbit response envelope ({}). The invocation ended without finishing its \
         contract — typically an agent that yielded mid-work — so this step's work is incomplete \
         and only what it persisted before stopping is durable.",
        bounded_diagnostic(error, redactor)
    )
}

fn declared_failure_diagnostic(
    status: &str,
    failure: Option<&orbit_agent::DeclaredResponseFailure>,
    redactor: &PatternRedactor,
) -> String {
    let prefix =
        format!("cli subprocess reported declared envelope status={status:?} despite exit 0");
    let Some(error) = failure.and_then(|failure| failure.error.as_ref()) else {
        return format!("{prefix}: declared envelope error details unavailable");
    };

    format!(
        "{prefix}: error.code={}; error.message={}",
        bounded_diagnostic(&error.code, redactor),
        bounded_diagnostic(&error.message, redactor),
    )
}

fn bounded_diagnostic(error: &str, redactor: &PatternRedactor) -> String {
    let redacted = redactor.apply_str(&redact_sensitive_env_text(error));
    let bounded: String = redacted
        .chars()
        .take(RESPONSE_DIAGNOSTIC_LIMIT_CHARS)
        .collect();
    let suffix = if bounded.len() < redacted.len() {
        "…"
    } else {
        ""
    };
    format!("{bounded}{suffix}")
}

struct StdoutTextPreview {
    text: String,
    truncated: bool,
    preview_bytes: usize,
}

fn stdout_text_preview(
    raw: &str,
    redactor: &PatternRedactor,
    prefer_tail: bool,
) -> StdoutTextPreview {
    let redacted = redactor.apply_str(&redact_sensitive_env_text(raw));
    let truncated = redacted.len() > STDOUT_TEXT_PREVIEW_LIMIT_BYTES;
    let text = if truncated {
        if prefer_tail {
            let requested_start = redacted.len() - STDOUT_TEXT_PREVIEW_LIMIT_BYTES;
            let boundary = redacted
                .char_indices()
                .map(|(idx, _)| idx)
                .find(|idx| *idx >= requested_start)
                .unwrap_or(redacted.len());
            let line_boundary = redacted[boundary..]
                .find('\n')
                .map_or(boundary, |idx| boundary + idx + 1);
            redacted[line_boundary..].to_string()
        } else {
            let boundary = redacted
                .char_indices()
                .map(|(idx, _)| idx)
                .take_while(|idx| *idx <= STDOUT_TEXT_PREVIEW_LIMIT_BYTES)
                .last()
                .unwrap_or(0);
            redacted[..boundary].to_string()
        }
    } else {
        redacted
    };
    let preview_bytes = text.len();

    StdoutTextPreview {
        text,
        truncated,
        preview_bytes,
    }
}
