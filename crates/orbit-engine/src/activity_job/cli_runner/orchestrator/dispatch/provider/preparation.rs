//! Provider envelope and sandbox preparation.

use super::super::super::super::argv::{
    apply_provider_static_arg_fixups, neutralize_inner_sandbox,
};
use super::super::CliDispatchContext;
use super::super::preparation::PreparedCliDispatch;
use crate::activity_job::cli_runner::image_input::StagedImages;
use crate::activity_job::dispatcher::DispatchError;
use orbit_agent::{Agent, AgentConfig};
use orbit_types::workflow::ExecutorSandboxKind;
use orbit_types::workflow::activity_job::AgentLoopSpec;
use orbit_types::workflow::{V2AuditEventKind, render_tag_instructions};
use serde_json::Value;

pub(super) struct ProviderEnvelope {
    pub(super) envelope_spec: AgentLoopSpec,
    pub(super) staged_images: Option<StagedImages>,
    pub(super) pinned_recovery_input: Option<Value>,
}

pub(super) fn prepare_provider_envelope<'a>(
    context: CliDispatchContext<'a>,
    prepared: &PreparedCliDispatch<'_>,
) -> Result<ProviderEnvelope, DispatchError> {
    let CliDispatchContext {
        host,
        spec,
        run_id,
        activity_name,
        step_id,
        input,
        audit,
        ..
    } = context;
    let PreparedCliDispatch {
        inspection_tools,
        image_input,
        task_id,
        notice_instruction,
        notice_posting_instruction,
        source_checkout,
        ..
    } = prepared;
    let crate::activity_job::cli_runner::orchestrator::prepare::SourceCheckout {
        source_cwd,
        subprocess_cwd,
        inspection_input,
        ..
    } = source_checkout;
    let task_id = *task_id;
    let notice_instruction = *notice_instruction;
    let notice_posting_instruction = *notice_posting_instruction;
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
    if let Some(instruction) = notice_instruction {
        envelope_spec.instruction.push_str("\n\n");
        envelope_spec.instruction.push_str(instruction);
    }
    if let Some(instruction) = notice_posting_instruction {
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
    Ok(ProviderEnvelope {
        envelope_spec,
        staged_images,
        pinned_recovery_input,
    })
}

pub(super) fn rebuilt_envelope_input(input: &serde_json::Value) -> serde_json::Value {
    let mut rebuilt = input.clone();
    if let Some(root) = rebuilt.as_object_mut() {
        root.remove("conversation_ref");
        if let Some(brief) = root
            .get_mut("brief")
            .and_then(serde_json::Value::as_object_mut)
        {
            brief.insert(
                "continuity".to_string(),
                serde_json::Value::String("rebuilt".to_string()),
            );
            brief.insert("conversation_ref".to_string(), serde_json::Value::Null);
        }
    }
    rebuilt
}

/// Configure provider argv and native confinement before constructing its agent.
pub(super) fn prepare_provider_agent(
    context: CliDispatchContext<'_>,
    provider: &str,
    sandbox: Option<&crate::activity_job::dispatcher::ResolvedSandbox>,
    resolved_sandbox: Option<&crate::activity_job::dispatcher::ResolvedSandbox>,
    cli_executor: &mut crate::activity_job::dispatcher::ResolvedCliExecutor,
) -> Result<(orbit_agent::AgentConfig, orbit_agent::Agent), DispatchError> {
    let CliDispatchContext { host, spec, .. } = context;
    let mut provider_config = host.provider_cli_config(provider);

    // Provider-specific static-arg fixups that are independent of whether the
    // outer sandbox is active. Today this only rewrites Claude's `--debug-file`
    // value to an absolute path under the writable claude state dir, so the
    // log lands somewhere `denyModify: .orbit/**` does not block. See
    // T20260505-22.
    apply_provider_static_arg_fixups(provider, &mut cli_executor.args);

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
    if sandbox.is_some() || explicitly_off {
        neutralize_inner_sandbox(provider, &mut provider_config, &mut cli_executor.args);
    }

    // Config parse / agent construction failures are deterministic — the
    // same spec fails identically on every attempt, so they are classified
    // permanent and skip the step retry wrapper (ORB-10006).
    let config = AgentConfig::from_provider_and_cli_config(
        provider,
        cli_executor.command.clone(),
        spec.model.as_deref(),
        &provider_config,
    )
    .and_then(|config| config.with_reasoning_effort(spec.reasoning_effort))
    .map_err(|err| DispatchError::CliInvocationPermanent(format!("agent config: {err}")))?;
    let agent = Agent::new(&config)
        .map_err(|err| DispatchError::CliInvocationPermanent(format!("agent build: {err}")))?;

    Ok((config, agent))
}
