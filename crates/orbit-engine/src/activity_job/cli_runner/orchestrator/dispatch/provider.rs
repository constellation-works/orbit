use std::sync::Arc;

use orbit_agent::{Agent, AgentConfig, AgentOperation, AgentRequest};
use orbit_common::process::build_budget::WAIT_DIRECTORY_ENV;
use orbit_common::security::child_env::{
    ACTIVITY_MCP_TOOLSETS_ENV, MCP_MANAGED_REGISTRY_ROOT_ENV, MCP_MANAGED_WORKSPACE_ENV,
};
use orbit_common::security::redaction::argv_redactor;
use orbit_tools::plugin::BrokeredCaller;
use orbit_types::tool::McpToolset;
use orbit_types::workflow::activity_job::{EXPLORE_ACTIVITY, V2AuditEventKind};
use orbit_types::workflow::{ActivityToolDenyPolicy, ExecutorSandboxKind, render_tag_instructions};

use crate::activity_job::dispatcher::DispatchOutcome;
use crate::context::{PluginBrokerRun, ProvenanceEnv, provenance_env};

use super::super::super::super::dispatcher::DispatchError;
use super::super::super::super::workspace::WorktreeBoundaryGuard;
use super::super::super::argv::{
    apply_provider_runtime_arg_fixups, apply_provider_static_arg_fixups,
    apply_read_only_provider_sandbox, apply_trusted_host_provider_sandbox,
    codex_mcp_server_launch_args, neutralize_inner_sandbox, try_audit_argv_for_dispatch,
};
use super::super::super::envelope::cli_agent_envelope_json;
use super::super::super::launcher::{orbit_tool_env, resolve_provider_launcher};
use super::super::super::plugin_broker::RunPluginBroker;
use super::super::super::prompt_file::PromptFile;
use super::super::environment::{
    activity_deadline_env, add_browser_kit_env, provider_child_environment,
    require_claude_worker_credential, require_crew_env,
};
use super::super::policy::{activity_policy_env, drop_inherited_policy_env};
use super::super::prepare::{SourceCheckout, prepare_dispatch_sandbox, resolve_dispatch_sandbox};
use super::{CliDispatchContext, post_run, preparation, subprocess};

pub(super) fn dispatch_prepared_provider(
    context: CliDispatchContext<'_>,
    prepared: preparation::PreparedCliDispatch<'_>,
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
    let preparation::PreparedCliDispatch {
        provider,
        trusted_host,
        mut cli_executor,
        inspection_tools,
        image_input,
        timeout_seconds,
        wall_clock_timeout,
        stall_window,
        task_ids,
        task_id,
        tool_disallow_list,
        activity_tools,
        task_ctx,
        regression_risk_instruction,
        explore_brief_instruction,
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
    let resolved_sandbox = resolve_dispatch_sandbox(
        host,
        &provider,
        fs_profile,
        subprocess_cwd.as_deref(),
        trusted_host.as_ref(),
    )?;
    let prepared_sandbox = prepare_dispatch_sandbox(
        activity_name,
        trusted_host.as_ref(),
        resolved_sandbox.as_ref(),
    )?;
    let sandbox = prepared_sandbox.effective;

    let mut envelope_spec = spec.clone();
    envelope_spec.instruction = envelope_spec
        .envelope_instruction(host.mid_run_steering_enabled())
        .into_owned();
    if let Some(tools) = &inspection_tools {
        envelope_spec.instruction.push_str("\n\n");
        envelope_spec.instruction.push_str(tools.instruction);
    }
    // The canary and the requested images are staged under the run's own
    // scratch directory, which the agent's filesystem profile can read; the
    // prompt names their paths and the canary's code never leaves this host.
    let staged_images = image_input
        .as_ref()
        .map(|plan| {
            let root = subprocess_cwd
                .clone()
                .or_else(|| std::env::current_dir().ok())
                .ok_or_else(|| {
                    DispatchError::CliInvocationPermanent(
                        "cannot locate the image staging root".into(),
                    )
                })?;
            let scratch = orbit_common::fs::path::ensure_orbit_scratch_dir(&root)
                .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))?;
            plan.stage(
                &scratch,
                source_cwd.as_deref().or(subprocess_cwd.as_deref()),
            )
        })
        .transpose()?;
    if let Some(staged) = &staged_images {
        envelope_spec.instruction.push_str("\n\n");
        envelope_spec.instruction.push_str(&staged.instruction);
    }
    if let Some(instruction) = regression_risk_instruction {
        envelope_spec.instruction.push_str("\n\n");
        envelope_spec.instruction.push_str(instruction);
    }
    let explore_instruction = spec
        .explore_instruction
        .as_deref()
        .filter(|instruction| !instruction.trim().is_empty())
        .filter(|_| host.explore_delegation_available());
    if let Some(instruction) = explore_instruction {
        envelope_spec.instruction.push_str("\n\n");
        envelope_spec.instruction.push_str(instruction);
    }
    if let Some(instruction) = explore_brief_instruction {
        envelope_spec.instruction.push_str("\n\n");
        envelope_spec.instruction.push_str(instruction);
    }
    // `pr.agent_sections` / `pr.required_sections`: the implementer writes
    // their content to a task artifact `pr_open` reads.
    if activity_name == "agent_implement"
        && let Some(instruction) = host.pr_config().conventions.implement_instruction()
    {
        envelope_spec.instruction.push_str("\n\n");
        envelope_spec.instruction.push_str(&instruction);
    }
    // Trusted per-tag guidance goes last, after every built-in rule it may
    // not override. The audit row names what the prompt carried.
    let tag_instructions = host.tag_instructions_for_activity(run_id, activity_name, input)?;
    if !tag_instructions.is_empty() {
        envelope_spec.instruction.push_str("\n\n");
        envelope_spec
            .instruction
            .push_str(&render_tag_instructions(&tag_instructions));
        audit.emit_lossy(V2AuditEventKind::TagInstructionsApplied {
            step_id: step_id.to_string(),
            activity_name: activity_name.to_string(),
            task_id: task_id.unwrap_or_default().to_string(),
            instructions: tag_instructions
                .iter()
                .map(|instruction| instruction.record())
                .collect(),
        });
    }
    if let Some(corpus) = host.agent_corpus_for_activity(run_id, activity_name)? {
        envelope_spec.instruction.push_str("\n\n");
        envelope_spec.instruction.push_str(&corpus.text);
        audit.emit_lossy(V2AuditEventKind::AgentCorpusApplied {
            step_id: step_id.to_string(),
            activity_name: activity_name.to_string(),
            index: corpus.index,
            revision: corpus.revision,
            paths: corpus.rows.iter().map(|row| row.path.clone()).collect(),
            sha256: corpus.sha256,
            bytes: corpus.text.len(),
            truncated: corpus.truncated,
        });
    }
    // Direct dispatches build the recovery input themselves and never pass
    // through bind_recovery_context. Stamp the same host pin onto the
    // envelope the provider reads. Admission below still recomputes the chain
    // from the original input and does not see this copy.
    let mut pinned_recovery_input = None;
    if activity_name == "pr_conflict_recovery" && inspection_input.is_none() {
        let mut owned = input.clone();
        crate::executor::automation::vcs::apply_authenticated_published_head(
            host, run_id, &mut owned,
        );
        pinned_recovery_input = Some(owned);
    }
    let envelope_input = pinned_recovery_input
        .as_ref()
        .or(inspection_input.as_ref())
        .unwrap_or(input);
    let envelope_json = cli_agent_envelope_json(
        &envelope_spec,
        run_id,
        envelope_input,
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
    //   - muse: append `--disable-sandbox` (see `neutralize_inner_sandbox`).
    // An explicit executor opt-out must not silently restore the provider's
    // inner sandbox when preparation selects a bare process.
    let explicitly_off = resolved_sandbox
        .as_ref()
        .is_some_and(|sandbox| sandbox.kind == ExecutorSandboxKind::Off);
    // [ORB-16814] An explorer is read-only by contract. Wrapped by Orbit's
    // sandbox, its reviewer profile already denies every worktree write and a
    // second inner sandbox cannot nest (Seatbelt refuses it). Unwrapped,
    // including an explicit executor opt-out, a bare fallback and a wrapper
    // that cannot start on this host, the provider's own read-only sandbox is
    // the boundary instead of being neutralized. A provider with no such mode
    // must not start: the profile is not an OS boundary, and its native tools
    // could write the caller's worktree.
    if activity_name == EXPLORE_ACTIVITY && sandbox.is_none() {
        if !apply_read_only_provider_sandbox(&provider, &mut provider_config) {
            return Err(DispatchError::CliInvocationPermanent(format!(
                "refusing explorer provider `{provider}`: no enforced read-only sandbox \
                 (the outer executor sandbox is not enforced, and this provider has no \
                 read-only mode)"
            )));
        }
    } else if sandbox.is_some() || explicitly_off {
        neutralize_inner_sandbox(&provider, &mut provider_config, &mut cli_executor.args);
    }

    // Config parse / agent construction failures are deterministic — the
    // same spec fails identically on every attempt, so they are classified
    // permanent and skip the step retry wrapper (ORB-10006).
    let config = AgentConfig::from_provider_and_cli_config(
        &provider,
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

    let scratch_root = subprocess_cwd
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| {
            DispatchError::CliInvocationPermanent("cannot locate invocation scratch root".into())
        })?;
    let scratch = orbit_common::fs::path::ensure_orbit_scratch_dir(&scratch_root)
        .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))?;
    let _skill_root = super::super::skills::prepare_experiment_skill_root(
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
    let prompt_file = invocation
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
    // custom `--print-timeout` without duplicating it, and the spawn deadline
    // is known here. The provider's argv is fixed at launch, when no
    // build-admission credit has been earned yet, so the budget derives from
    // the runtime alone: the provider stops itself before Orbit's wall clock
    // kills it. [ORB-11337] [ORB-15206]
    let print_timeout =
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
    if let Some(tools) = inspection_tools {
        subprocess_args.extend(tools.args);
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
    let argv_redacted: Vec<String> = audit_argv.iter().map(|a| redaction.apply_str(a)).collect();

    // The prompt the provider receives, whether on stdin or in its file.
    let stdin_blob_ref = audit.write_blob(&invocation.stdin);

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
    dispatch_env.push((
        orbit_common::security::child_env::ACTIVITY_TIMEOUT_ENV.to_string(),
        timeout_seconds.saturating_mul(1000).to_string(),
    ));
    let build_wait_directory = tempfile::Builder::new()
        .prefix("build-waits-")
        .tempdir_in(&scratch)
        .map_err(|error| {
            DispatchError::CliInvocationPermanent(format!("create build wait channel: {error}"))
        })?;
    dispatch_env.push((
        WAIT_DIRECTORY_ENV.to_string(),
        build_wait_directory.path().display().to_string(),
    ));
    dispatch_env.extend(activity_policy_env(
        spec,
        activity_name,
        tool_disallow_list.as_deref(),
        &activity_tools.effective_tools,
        fs_profile,
    ));
    // A run offered explorer delegation gets `orbit.agent.invoke` on its
    // nested MCP server, which a default server does not advertise.
    if explore_instruction.is_some() {
        dispatch_env.push((
            ACTIVITY_MCP_TOOLSETS_ENV.to_string(),
            McpToolset::AgentInvoke.as_str().to_string(),
        ));
    }
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
    // Canonical before the provider starts, so the Linux post-run guard's
    // scratch exemption cannot be redirected by the child.
    let scratch_dir = match subprocess_cwd.as_ref() {
        Some(cwd) => {
            let scratch =
                orbit_common::fs::path::ensure_orbit_scratch_dir(cwd).map_err(|error| {
                    DispatchError::CliInvocationPermanent(format!(
                        "failed to create worker scratch dir: {error}"
                    ))
                })?;
            dispatch_env.push((
                orbit_common::fs::path::ORBIT_SCRATCH_DIR_ENV.to_string(),
                scratch.display().to_string(),
            ));
            Some(scratch)
        }
        None => None,
    };
    require_crew_env(&spec.crew_env, &child_env)?;
    require_claude_worker_credential(host, &provider, &child_env)?;
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
    add_browser_kit_env(&mut child_env);
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
    let started_at = chrono::Utc::now();
    let process_exit = subprocess::invoke_cli_provider(subprocess::CliProviderInvocation {
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
