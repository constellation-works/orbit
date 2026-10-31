use std::sync::Arc;
use std::time::Duration;

use orbit_tools::ToolContext;
use orbit_types::workflow::activity_job::{AgentLoopSpec, TrustedHostAdmission, V2AuditEventKind};
use orbit_types::workflow::{
    ExperimentActivityOverlay, ExperimentOverlayRecord, ExploreBriefAdmission,
};
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
    require_conflict_recovery_target, trusted_host_admission,
};
use super::CliDispatchContext;

pub(super) struct PreparedCliDispatch<'a> {
    pub(super) provider: String,
    pub(super) trusted_host: Option<TrustedHostAdmission>,
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
    pub(super) regression_risk_instruction: Option<&'a str>,
    pub(super) explore_brief_instruction: Option<&'a str>,
    pub(super) tool_ctx: ToolContext,
    pub(super) declared_worktree_pair: Option<DeclaredWorktreePair>,
    pub(super) source_checkout: SourceCheckout,
    pub(super) experiment_overlays: Vec<ExperimentActivityOverlay>,
    pub(super) experiment_records: Vec<ExperimentOverlayRecord>,
}

/// Add the host's `regression_risk` marks to the task snapshot of an
/// activity that declares how to act on them, and return that instruction
/// when it did. An activity without one, a prompt with no task snapshot, or
/// a task with nothing marked leaves the snapshot and instruction as they
/// are.
fn attach_regression_risk<'a>(
    host: &dyn RuntimeHost,
    spec: &'a AgentLoopSpec,
    run_id: &str,
    activity_name: &str,
    input: &Value,
    task_ctx: Option<&mut Value>,
) -> Option<&'a str> {
    let instruction = spec
        .regression_risk_instruction
        .as_deref()
        .filter(|instruction| !instruction.trim().is_empty())?;
    let context = task_ctx?.as_object_mut()?;
    let marks = host.regression_risk_for_agent_input(run_id, activity_name, input)?;
    context.insert("regression_risk".to_string(), marks);
    Some(instruction)
}

/// Ask the host for the task's explore brief when `spec` declares an
/// `explore_brief_instruction`, record the outcome whatever it is, and put a
/// delivered brief into the task context. Returns the instruction to append
/// only when the prompt carries the brief.
#[allow(clippy::too_many_arguments)]
fn attach_explore_brief<'a>(
    host: &dyn RuntimeHost,
    spec: &'a AgentLoopSpec,
    run_id: &str,
    step_id: &str,
    activity_name: &str,
    input: &Value,
    task_ctx: Option<&mut Value>,
    audit: &V2AuditWriter,
) -> Option<&'a str> {
    let instruction = spec
        .explore_brief_instruction
        .as_deref()
        .filter(|instruction| !instruction.trim().is_empty())?;
    let ExploreBriefAdmission { mut record, brief } =
        host.explore_brief_for_activity(run_id, activity_name, input)?;
    let delivered = match (brief, task_ctx.and_then(Value::as_object_mut)) {
        (Some(brief), Some(context)) if record.deliverable() => {
            context.insert("explore_brief".to_string(), brief);
            true
        }
        _ => false,
    };
    record.delivered = delivered;
    audit.emit_lossy(V2AuditEventKind::ExploreBriefApplied {
        step_id: step_id.to_string(),
        activity_name: activity_name.to_string(),
        task_id: task_id_from_input(input).unwrap_or_default().to_string(),
        brief: record,
    });
    delivered.then_some(instruction)
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
    // A read-only reviewer edits nothing; any other agent may, so it first
    // holds the context locks its run's admission deferred.
    if fs_profile != Some("reviewer") {
        host.acquire_deferred_footprint(run_id, activity_name)?;
    }
    let provider = spec.provider.as_str().to_string();
    let trusted_host = trusted_host_admission(spec, activity_name, input)?;
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
    let timeout_seconds =
        invocation_timeout_seconds(spec, activity_name, trusted_host.as_ref(), input);
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
    let regression_risk_instruction =
        attach_regression_risk(host, spec, run_id, activity_name, input, task_ctx.as_mut());
    let explore_brief_instruction = attach_explore_brief(
        host,
        spec,
        run_id,
        step_id,
        activity_name,
        input,
        task_ctx.as_mut(),
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
            trusted_host,
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
            regression_risk_instruction,
            explore_brief_instruction,
            tool_ctx,
            declared_worktree_pair,
            source_checkout,
            experiment_overlays,
            experiment_records,
        },
    )
}
