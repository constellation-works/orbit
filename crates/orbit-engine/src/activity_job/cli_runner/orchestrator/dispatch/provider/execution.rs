//! Provider invocation, execution and continuity fallback.

use std::sync::Arc;

use orbit_agent::{AgentOperation, AgentRequest};
use orbit_common::security::redaction::argv_redactor;
use orbit_tools::plugin::BrokeredCaller;
use orbit_types::workflow::ActivityToolDenyPolicy;
use orbit_types::workflow::activity_job::V2AuditEventKind;

use crate::activity_job::dispatcher::DispatchOutcome;
use crate::context::PluginBrokerRun;

use super::super::super::super::super::dispatcher::DispatchError;
use super::super::super::super::super::workspace::WorktreeBoundaryGuard;
use super::super::super::super::argv::{
    apply_provider_runtime_arg_fixups, codex_mcp_server_launch_args, try_audit_argv_for_dispatch,
};
use super::super::super::super::envelope::cli_agent_envelope_json;
use super::super::super::super::launcher::{orbit_tool_env, resolve_provider_launcher};
use super::super::super::super::plugin_broker::RunPluginBroker;
use super::super::super::super::prompt_file::PromptFile;
use super::super::super::environment::provider_child_environment;
use super::super::super::prepare::{
    SourceCheckout, prepare_dispatch_sandbox, resolve_dispatch_sandbox,
};
use super::super::{CliDispatchContext, post_run, preparation as dispatch_preparation, subprocess};

use super::{environment, preparation};
pub(in super::super) fn dispatch_prepared_provider(
    context: CliDispatchContext<'_>,
    prepared: dispatch_preparation::PreparedCliDispatch<'_>,
) -> Result<DispatchOutcome, DispatchError> {
    let CliDispatchContext {
        host,
        spec,
        step_id,
        activity_name,
        run_id,
        audit,
        input,
        fs_profile,
    } = context;
    let resolved_sandbox = resolve_dispatch_sandbox(
        host,
        &prepared.provider,
        fs_profile,
        prepared.source_checkout.subprocess_cwd.as_deref(),
    )?;
    let prepared_sandbox = prepare_dispatch_sandbox(resolved_sandbox.as_ref())?;
    let sandbox = prepared_sandbox.effective;

    let preparation::ProviderEnvelope {
        envelope_spec,
        staged_images,
        pinned_recovery_input,
    } = preparation::prepare_provider_envelope(context, &prepared)?;
    let dispatch_preparation::PreparedCliDispatch {
        provider,
        mut cli_executor,
        inspection_tools,
        image_input: _,
        timeout_seconds,
        wall_clock_timeout,
        stall_window,
        task_ids,
        task_id,
        tool_disallow_list,
        activity_tools,
        task_ctx,
        notice_instruction: _,
        notice_posting_instruction: _,
        tool_ctx,
        declared_worktree_pair,
        source_checkout,
        experiment_overlays,
        experiment_records,
    } = prepared;
    let SourceCheckout {
        source_cwd,
        inspection,
        inspection_input,
        inspection_task_ctx,
        subprocess_cwd,
        subprocess_cwd_string,
    } = source_checkout;
    // At most one live arm delivers memory: the memory surface is one
    // experiment's per task (`surface_conflict`).
    let crew_memory = experiment_overlays
        .iter()
        .find_map(|overlay| overlay.memory.as_ref())
        .map(|delivery| &delivery.block);
    let envelope_input = pinned_recovery_input
        .as_ref()
        .or(inspection_input.as_ref())
        .unwrap_or(input);

    let (config, agent) = preparation::prepare_provider_agent(
        context,
        &provider,
        sandbox,
        resolved_sandbox.as_ref(),
        &mut cli_executor,
    )?;

    let continuity_off = envelope_input
        .pointer("/brief/continuity")
        .and_then(serde_json::Value::as_str)
        == Some("off")
        || envelope_input
            .pointer("/brief/mandate/continuity")
            .and_then(serde_json::Value::as_str)
            == Some("off");
    let requested_conversation_ref = (!continuity_off)
        .then(|| {
            envelope_input
                .get("conversation_ref")
                .or_else(|| envelope_input.pointer("/brief/conversation_ref"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .flatten();
    let mut normalized_continuity_input = None;
    let active_conversation_ref = match requested_conversation_ref {
        Some(conversation_ref) if agent.supports_continuity() => Some(conversation_ref),
        Some(_) => {
            let _ = host.record_resident_continuity_rebuilt(run_id);
            normalized_continuity_input = Some(preparation::rebuilt_envelope_input(envelope_input));
            None
        }
        None => None,
    };
    let effective_envelope_input = normalized_continuity_input
        .as_ref()
        .unwrap_or(envelope_input);
    let envelope_json = cli_agent_envelope_json(
        &envelope_spec,
        run_id,
        effective_envelope_input,
        inspection_task_ctx.as_ref().or(task_ctx.as_ref()),
        &activity_tools.requested_tools,
        &activity_tools.effective_tools,
        crew_memory,
    )?;

    let agent_req = AgentRequest {
        operation: AgentOperation::Activity {
            activity_id: "v2_cli_backend".to_string(),
        },
        envelope_json,
        verbose: false,
        conversation_ref: active_conversation_ref.clone(),
    };

    // `invoke` only renders the argv/stdin for the subprocess (nothing has
    // executed yet) — failures here are deterministic request-shaping errors.
    let (mut invocation, _trace) = agent
        .invoke(agent_req)
        .map_err(|err| DispatchError::CliInvocationPermanent(format!("agent invoke: {err}")))?;
    let model = agent.model_name().map(str::to_string);
    let resolved_program =
        resolve_provider_launcher(&provider, &invocation.program, subprocess_cwd.as_deref())
            .map_err(|err| DispatchError::CliInvocationPermanent(err.message))?;
    // The child's whole environment is composed from here and applied to a
    // cleared one by every launcher, so the `[execution.env]` allowlist governs what an
    // untrusted provider subprocess can read. The provider's declared
    // `required_env_vars` ride along as extras so a strict allowlist still
    // starts the CLI. `dispatch_env` is appended last and later entries win, so
    // this run's identity and tool pinning override any same-named value the
    // allowlist forwarded from an outer process. [ORB-10917] Tool pinning keeps
    // this environment's PATH, `workflow.validation_env.path` first. [ORB-15204]
    let mut child_env = provider_child_environment(
        host,
        &provider,
        sandbox,
        &spec.crew_env,
        invocation.required_env_vars,
    );
    let orbit_env = orbit_tool_env(&child_env)
        .map_err(|error| DispatchError::CliInvocationPermanent(error.message))?;
    let codex_mcp_args = if provider == "codex" {
        let orbit_bin = orbit_env
            .iter()
            .find(|(name, _)| name == "ORBIT_BIN")
            .map(|(_, value)| value.as_str())
            .ok_or_else(|| {
                DispatchError::CliInvocationPermanent("managed ORBIT_BIN missing".into())
            })?;
        Some(codex_mcp_server_launch_args(orbit_bin).map_err(|error| {
            DispatchError::CliInvocationPermanent(format!("encode codex MCP command: {error}"))
        })?)
    } else {
        None
    };

    let scratch_root = subprocess_cwd
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| {
            DispatchError::CliInvocationPermanent("cannot locate invocation scratch root".into())
        })?;
    let scratch = orbit_common::fs::path::ensure_orbit_scratch_dir(&scratch_root)
        .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))?;
    let _skill_root = super::super::super::skills::prepare_experiment_skill_root(
        &provider,
        &experiment_overlays,
        &scratch,
        &mut child_env,
    )?;
    if !experiment_records.is_empty() {
        audit.emit_lossy(V2AuditEventKind::ExperimentOverlayApplied {
            step_id: step_id.to_string(),
            activity_name: activity_name.to_string(),
            task_id: task_id.unwrap_or_default().to_string(),
            overlays: experiment_records,
        });
    }
    // [ORB-16577] A CLI that cannot read stdin gets its prompt through a
    // private file in this run's scratch directory instead, so the envelope
    // still never reaches argv. The file is deleted when dispatch returns.
    let mut prompt_file = invocation
        .prompt_file_flag
        .map(|flag| PromptFile::write(&scratch, &invocation.stdin).map(|file| (flag, file)))
        .transpose()?;
    let stdin_bytes: &[u8] = if prompt_file.is_some() {
        &[]
    } else {
        &invocation.stdin
    };

    let mut subprocess_args = Vec::with_capacity(cli_executor.args.len() + invocation.args.len());
    subprocess_args.extend(cli_executor.args.iter().cloned());
    subprocess_args.extend(invocation.args.iter().cloned());
    if let Some((flag, file)) = &prompt_file {
        subprocess_args.push((*flag).to_string());
        subprocess_args.push(file.argument().to_string());
    }
    if let Some(mcp_args) = &codex_mcp_args {
        subprocess_args.extend(mcp_args.iter().cloned());
    }
    // Combined executor + transport argv is the only place that can honor a
    // custom `--print-timeout` without duplicating it, and the spawn deadline
    // is known here. The provider's argv is fixed at launch, when no
    // build-admission credit has been earned yet, so the budget derives from
    // the runtime alone: the provider stops itself before Orbit's wall clock
    // kills it. [ORB-11337] [ORB-15206]
    let mut print_timeout =
        apply_provider_runtime_arg_fixups(&provider, &mut subprocess_args, wall_clock_timeout);
    if let Some(budget) = print_timeout {
        tracing::info!(
            provider = %provider,
            run_id,
            print_timeout_ms = budget.as_millis() as u64,
            effective_deadline_ms = wall_clock_timeout.as_millis() as u64,
            "injected provider print timeout"
        );
    }
    if let Some(tools) = &inspection_tools {
        subprocess_args.extend(tools.args.iter().cloned());
    }
    if let Some(staged) = &staged_images {
        subprocess_args.extend(staged.args.iter().cloned());
    }

    // The audit argv reflects what actually runs. Under sandbox-exec the
    // parent is `<trusted sandbox-exec> -p <profile> <program> <args...>`;
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
    let mut argv_redacted: Vec<String> =
        audit_argv.iter().map(|a| redaction.apply_str(a)).collect();

    // The prompt the provider receives, whether on stdin or in its file.
    let mut stdin_blob_ref = audit.write_blob(&invocation.stdin);

    // L-0095: Provider cwd is advisory; enforce the linked-worktree postcondition.
    // Inspection owns its temporary checkout separately; retain the original
    // source pair here so task-worktree ownership validation stays unchanged.
    // Snapshot both sides of a linked-worktree invocation immediately before
    // provider spawn. `tool_ctx.workspace_root` is the registered primary
    // checkout; `subprocess_cwd` is the canonical assigned worktree. Direct
    // invocations where those resolve to the same checkout remain unchanged.
    // The primary's baseline is the workspace base branch, never whichever
    // branch it has checked out [ORB-19177].
    let mut worktree_boundary = WorktreeBoundaryGuard::capture(
        input,
        task_ctx.as_ref(),
        run_id,
        &provider,
        source_cwd.as_deref(),
        tool_ctx.workspace_root.as_deref(),
        declared_worktree_pair.as_ref(),
        &host.workspace_base_branch(),
    )?
    // A violation's full fingerprints belong in the run's blob store, not in
    // the error string every downstream reader copies [ORB-12467].
    .map(|boundary| {
        boundary
            .with_audit(Arc::clone(audit))
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

    let model_redacted = agent.model_name().map(|m| redaction.apply_str(m));
    let task_scope_blob_ref = task_ctx.as_ref().and_then(|task| {
        let scope: serde_json::Map<String, serde_json::Value> = [
            "id",
            "title",
            "description",
            "acceptance_criteria",
            "context_files",
            "admission_tier",
            "admission_type",
            "final_validation_commands",
        ]
        .into_iter()
        .filter_map(|key| task.get(key).map(|value| (key.into(), value.clone())))
        .collect();
        serde_json::to_vec(&scope)
            .ok()
            .filter(|bytes| bytes.len() <= 65_536)
            .and_then(|bytes| audit.write_blob(&bytes))
    });
    audit.emit_lossy(V2AuditEventKind::CliInvocationStarted {
        provider: provider.clone(),
        reasoning_effort: config.reasoning_effort.map(|effort| effort.to_string()),
        configuration_fingerprint: serde_json::to_vec(spec).ok().map(|bytes| {
            use sha2::{Digest, Sha256};
            format!(
                "sha256:{}",
                orbit_types::identity::lower_hex(Sha256::digest(bytes))
            )
        }),
        task_scope_blob_ref,
        argv_redacted: argv_redacted.clone(),
        stdin_blob_ref: stdin_blob_ref.clone(),
        model: model_redacted,
        cwd: subprocess_cwd_string.clone(),
        wall_clock_timeout_ms: wall_clock_timeout.as_millis() as u64,
        print_timeout_ms: print_timeout.map(|budget| budget.as_millis() as u64),
        effective_deadline_ms: Some(wall_clock_timeout.as_millis() as u64),
        sandbox_backend: prepared_sandbox.metadata.backend.clone(),
        sandbox_trusted_wrapper: prepared_sandbox.metadata.trusted_wrapper.clone(),
        sandbox_probe_outcome: prepared_sandbox.metadata.probe_outcome.clone(),
        sandbox_write_enforcement: Some(prepared_sandbox.metadata.write_enforcement.clone()),
        sandbox_read_enforcement: Some(prepared_sandbox.metadata.read_enforcement.clone()),
    });

    let environment::PreparedProviderEnvironment {
        scratch_dir,
        build_wait_directory,
    } = environment::prepare_provider_environment(environment::ProviderEnvironmentInput {
        context,
        provider: &provider,
        model: &model,
        inspection: &inspection,
        tool_ctx: &tool_ctx,
        task_id,
        wall_clock_timeout,
        timeout_seconds,
        scratch: &scratch,
        tool_disallow_list: &tool_disallow_list,
        activity_tools: &activity_tools,
        orbit_env,
        subprocess_cwd: &subprocess_cwd,
        sandbox,
        child_env: &mut child_env,
        fixed_env: invocation.fixed_env,
    })?;
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
    let agent_execution_id = host
        .begin_agent_execution(
            run_id,
            step_id,
            &provider,
            model.as_deref(),
            &orbit_types::telemetry::AgentExecutionIdentity {
                crew: spec.crew_name.clone(),
                reasoning_effort: config.reasoning_effort,
            },
        )
        .map_err(|error| {
            DispatchError::CliInvocationPermanent(format!("record agent execution: {error}"))
        })?;
    let mut started_at = chrono::Utc::now();
    let mut process_exit = subprocess::invoke_cli_provider(subprocess::CliProviderInvocation {
        context,
        provider: &provider,
        resolved_program: &resolved_program,
        subprocess_args: &subprocess_args,
        child_env: &child_env,
        subprocess_cwd: &subprocess_cwd,
        subprocess_cwd_string: &subprocess_cwd_string,
        sandbox,
        stdin_bytes,
        wall_clock_timeout,
        timeout_seconds,
        stall_window,
        build_wait_directory: &build_wait_directory,
        redaction,
        cli_executor: &cli_executor,
        plugin_broker,
        worktree_boundary: &mut worktree_boundary,
        task_ids: &task_ids,
    })?;
    if active_conversation_ref.is_some()
        && !process_exit.timed_out
        && !process_exit.stalled
        && agent.resume_failed(
            process_exit.exit_code,
            process_exit.stdout.bytes(),
            process_exit.stderr.bytes(),
        )
    {
        let _ = host.record_resident_continuity_rebuilt(run_id);
        let fallback_input = preparation::rebuilt_envelope_input(envelope_input);
        let fallback_envelope_json = cli_agent_envelope_json(
            &envelope_spec,
            run_id,
            &fallback_input,
            inspection_task_ctx.as_ref().or(task_ctx.as_ref()),
            &activity_tools.requested_tools,
            &activity_tools.effective_tools,
            crew_memory,
        )?;
        let (fallback_invocation, _trace) = agent
            .invoke(AgentRequest {
                operation: AgentOperation::Activity {
                    activity_id: "v2_cli_backend".to_string(),
                },
                envelope_json: fallback_envelope_json,
                verbose: false,
                conversation_ref: None,
            })
            .map_err(|err| {
                DispatchError::CliInvocationPermanent(format!("agent fallback invoke: {err}"))
            })?;
        invocation = fallback_invocation;
        prompt_file = invocation
            .prompt_file_flag
            .map(|flag| PromptFile::write(&scratch, &invocation.stdin).map(|file| (flag, file)))
            .transpose()?;
        let fallback_stdin_bytes: &[u8] = if prompt_file.is_some() {
            &[]
        } else {
            &invocation.stdin
        };
        subprocess_args.clear();
        subprocess_args.extend(cli_executor.args.iter().cloned());
        subprocess_args.extend(invocation.args.iter().cloned());
        if let Some((flag, file)) = &prompt_file {
            subprocess_args.push((*flag).to_string());
            subprocess_args.push(file.argument().to_string());
        }
        if let Some(mcp_args) = &codex_mcp_args {
            subprocess_args.extend(mcp_args.iter().cloned());
        }
        print_timeout =
            apply_provider_runtime_arg_fixups(&provider, &mut subprocess_args, wall_clock_timeout);
        if let Some(tools) = &inspection_tools {
            subprocess_args.extend(tools.args.iter().cloned());
        }
        if let Some(staged) = &staged_images {
            subprocess_args.extend(staged.args.iter().cloned());
        }
        let fallback_audit_argv = try_audit_argv_for_dispatch(
            &resolved_program,
            &subprocess_args,
            sandbox,
            subprocess_cwd.as_deref(),
        )
        .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))?;
        argv_redacted = fallback_audit_argv
            .iter()
            .map(|a| redaction.apply_str(a))
            .collect();
        stdin_blob_ref = audit.write_blob(&invocation.stdin);
        let fallback_broker = RunPluginBroker::start(host, run_id, broker_run.as_ref(), sandbox);
        child_env.retain(|(key, _)| {
            key != crate::context::PLUGIN_BROKER_ENV && key != "ORBIT_ACTIVITY_RESIDENT_CONTINUITY"
        });
        child_env.push((
            "ORBIT_ACTIVITY_RESIDENT_CONTINUITY".to_string(),
            "rebuilt".to_string(),
        ));
        child_env.extend(fallback_broker.env());
        started_at = chrono::Utc::now();
        process_exit = subprocess::invoke_cli_provider(subprocess::CliProviderInvocation {
            context,
            provider: &provider,
            resolved_program: &resolved_program,
            subprocess_args: &subprocess_args,
            child_env: &child_env,
            subprocess_cwd: &subprocess_cwd,
            subprocess_cwd_string: &subprocess_cwd_string,
            sandbox,
            stdin_bytes: fallback_stdin_bytes,
            wall_clock_timeout,
            timeout_seconds,
            stall_window,
            build_wait_directory: &build_wait_directory,
            redaction,
            cli_executor: &cli_executor,
            plugin_broker: fallback_broker,
            worktree_boundary: &mut worktree_boundary,
            task_ids: &task_ids,
        })?;
    }
    post_run::finish_cli_dispatch(post_run::CliProviderCompletion {
        context,
        started_at,
        provider,
        model,
        agent_execution_id,
        task_ids: &task_ids,
        worktree_boundary,
        sandbox,
        subprocess_cwd: subprocess_cwd.as_deref(),
        redaction,
        timeout_seconds,
        argv_redacted,
        stdin_blob_ref,
        print_timeout,
        stall_window,
        inspection: inspection.as_ref(),
        staged_images: staged_images.as_ref(),
        scratch_dir: scratch_dir.as_deref(),
        build_wait_directory: &build_wait_directory,
        process_exit,
    })
}
