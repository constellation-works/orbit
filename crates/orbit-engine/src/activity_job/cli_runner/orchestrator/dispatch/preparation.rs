use std::sync::Arc;
use std::time::Duration;

use orbit_tools::ToolContext;
use orbit_types::workflow::activity_job::{AgentLoopSpec, V2AuditEventKind};
use orbit_types::workflow::{ExperimentActivityOverlay, ExperimentOverlayRecord};
use serde_json::Value;

use crate::activity_job::cli_runner::image_input::{ImageInputPlan, admit_image_input};
use crate::activity_job::cli_runner::inspection::InspectionTools;
use crate::activity_job::dispatcher::ResolvedCliExecutor;
use crate::activity_job::workspace::DeclaredWorktreePair;
use crate::context::{ResolvedActivityTools, RuntimeHost};
use crate::executor::automation::vcs::git::{GitTimeoutBudget, GitTimeoutBudgetGuard};

use super::super::super::super::audit_writer::V2AuditWriter;
use super::super::super::super::dispatcher::{DispatchError, DispatchOutcome};
use super::super::super::super::workspace::validate_declared_worktree_pair;
use super::super::super::envelope::{task_id_from_input, task_ids_from_input};
use super::super::super::inspection::{admit_inspection, prepare_inspection_tools};
use super::super::policy::{
    ActivityToolGrant, apply_experiment_tool_overlays, resolve_activity_tool_grant,
};
use super::super::prepare::{
    SourceCheckout, activity_tool_context, invocation_timeout_seconds, prepare_source_checkout,
    reject_removed_trusted_host_execution, require_conflict_recovery_target,
};
use super::CliDispatchContext;

pub(super) struct PreparedCliDispatch<'a> {
    pub(super) provider: String,
    pub(super) cli_executor: ResolvedCliExecutor,
    pub(super) inspection_tools: Option<InspectionTools>,
    pub(super) image_input: Option<ImageInputPlan>,
    pub(super) timeout_seconds: u64,
    pub(super) wall_clock_timeout: Duration,
    pub(super) stall_window: Option<Duration>,
    pub(super) task_ids: Vec<String>,
    pub(super) task_id: Option<&'a str>,
    pub(super) tool_disallow_list: Option<Vec<String>>,
    pub(super) activity_tools: ResolvedActivityTools,
    pub(super) task_ctx: Option<Value>,
    pub(super) notice_instruction: Option<&'a str>,
    pub(super) notice_posting_instruction: Option<&'a str>,
    pub(super) tool_ctx: ToolContext,
    pub(super) declared_worktree_pair: Option<DeclaredWorktreePair>,
    pub(super) source_checkout: SourceCheckout,
    pub(super) experiment_overlays: Vec<ExperimentActivityOverlay>,
    pub(super) experiment_records: Vec<ExperimentOverlayRecord>,
}

/// The notice instructions one dispatch carries: the untrusted-data
/// handling, only when a brief rides in the prompt, and the posting
/// guidance, only when this dispatch may call `orbit.notice.post`.
pub(super) struct FleetNoticeInstructions<'a> {
    pub(super) notice_instruction: Option<&'a str>,
    pub(super) notice_posting_instruction: Option<&'a str>,
}

/// Ask the host for the workspace's open fleet notices when `spec` declares
/// a `notice_instruction`, put the brief into the task context and record
/// what was delivered. Notices are workspace-scoped, so a prompt with no
/// task snapshot (a multi-task pilot) gains a task object holding only the
/// block. The untrusted-data instruction rides only when the prompt
/// carries the block; the posting instruction is independent of the brief
/// and rides whenever the resolved callable set carries
/// `orbit.notice.post`, so an empty board never suppresses it.
#[allow(
    clippy::too_many_arguments,
    reason = "notice delivery needs the step identity, the mutable task context and the audit writer together"
)]
fn attach_fleet_notices<'a>(
    host: &dyn RuntimeHost,
    spec: &'a AgentLoopSpec,
    run_id: &str,
    step_id: &str,
    activity_name: &str,
    input: &Value,
    task_ctx: &mut Option<Value>,
    effective_tools: &[String],
    audit: &V2AuditWriter,
) -> FleetNoticeInstructions<'a> {
    let notice_posting_instruction = spec
        .notice_posting_instruction
        .as_deref()
        .filter(|instruction| !instruction.trim().is_empty())
        .filter(|_| {
            effective_tools
                .iter()
                .any(|tool| tool == "orbit.notice.post")
        });
    let Some(instruction) = spec
        .notice_instruction
        .as_deref()
        .filter(|instruction| !instruction.trim().is_empty())
    else {
        return FleetNoticeInstructions {
            notice_instruction: None,
            notice_posting_instruction,
        };
    };
    let Some(brief) = host.notices_for_agent_input(run_id, activity_name, input) else {
        return FleetNoticeInstructions {
            notice_instruction: None,
            notice_posting_instruction,
        };
    };
    if task_ctx.is_none() {
        *task_ctx = Some(Value::Object(serde_json::Map::new()));
    }
    let Some(context) = task_ctx.as_mut().and_then(Value::as_object_mut) else {
        return FleetNoticeInstructions {
            notice_instruction: None,
            notice_posting_instruction,
        };
    };
    context.insert("fleet_notices".to_string(), brief.block);
    audit.emit_lossy(V2AuditEventKind::NoticesDelivered {
        step_id: step_id.to_string(),
        activity_name: activity_name.to_string(),
        task_id: task_id_from_input(input).unwrap_or_default().to_string(),
        notices: brief.notices,
        omitted: brief.omitted,
    });
    FleetNoticeInstructions {
        notice_instruction: Some(instruction),
        notice_posting_instruction,
    }
}

/// Run a catalog activity on behalf of a pipeline step whose id differs from
/// the activity name. Policy and broker authorization use `activity_name`;
/// audit labels and worktree-boundary reports use `step_id`.
/// One provider invocation of a step: admission, preparation, the
/// subprocess and its completion projection. The step entry
/// (`continuation::run_cli_backend_for_step`) may call it again for a
/// bounded implementation continuation [ORB-21837].
#[allow(
    clippy::too_many_arguments,
    reason = "policy authorizes the activity name while audit and the worktree boundary report the step id"
)]
pub(crate) fn dispatch_cli_invocation(
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
    // A read-only reviewer edits nothing; any other agent may, so it first
    // holds the context locks its run's admission deferred.
    if fs_profile != Some("reviewer") {
        host.acquire_deferred_footprint(run_id, activity_name)?;
    }
    let provider = spec.provider.as_str().to_string();
    reject_removed_trusted_host_execution(spec, activity_name)?;
    // Reject an inspection configuration that cannot start (a non-reviewer
    // profile, a provider with no CLI runtime) before executor lookup can
    // classify it as a transient missing-CLI error.
    admit_inspection(&provider, input, fs_profile)?;
    let cli_executor = host.resolve_cli_executor(&provider)?;
    let inspection_tools =
        prepare_inspection_tools(&provider, input, cli_executor.inspection_args.as_deref())?;
    // Like inspection, an image-input run its lane or crew cannot serve is
    // refused before any session or checkout exists.
    let image_input = admit_image_input(&provider, spec, input, cli_executor.image_input.as_ref())?;
    let timeout_seconds = invocation_timeout_seconds(spec);
    let wall_clock_timeout = Duration::from_secs(timeout_seconds);
    let stall_window = host.agent_stall_window();

    let task_ids = task_ids_from_input(input);
    let task_id = task_id_from_input(input);
    require_conflict_recovery_target(activity_name, &task_ids, input, run_id)?;
    let mut grant = resolve_activity_tool_grant(host, spec, activity_name, input, &task_ids)?;
    // A live experiment arm's tool delta applies after the grant, so the
    // audit row below and the child's policy envelope carry the treatment.
    let experiment_overlays =
        host.experiment_overlays_for_activity(run_id, activity_name, input)?;
    let experiment_records = apply_experiment_tool_overlays(
        host,
        &mut grant,
        &experiment_overlays,
        activity_name,
        input,
        &task_ids,
    )?;
    let ActivityToolGrant {
        tool_policy,
        tool_disallow_list,
        activity_tools,
    } = grant;

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
        omitted_requirement_notes: activity_tools.omitted_requirement_notes.clone(),
    });

    let mut task_ctx = host.task_context_for_agent_input(run_id, step_id, input)?;
    let FleetNoticeInstructions {
        notice_instruction,
        notice_posting_instruction,
    } = attach_fleet_notices(
        host,
        spec,
        run_id,
        step_id,
        activity_name,
        input,
        &mut task_ctx,
        &activity_tools.effective_tools,
        &audit,
    );
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
    let source_checkout = prepare_source_checkout(
        input,
        task_ctx.as_ref(),
        tool_ctx.workspace_root.as_deref(),
        fs_profile,
    )?;
    super::provider::dispatch_prepared_provider(
        CliDispatchContext {
            host,
            spec,
            step_id,
            activity_name,
            run_id,
            audit: &audit,
            input,
            fs_profile,
        },
        PreparedCliDispatch {
            provider,
            cli_executor,
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
            notice_instruction,
            notice_posting_instruction,
            tool_ctx,
            declared_worktree_pair,
            source_checkout,
            experiment_overlays,
            experiment_records,
        },
    )
}
