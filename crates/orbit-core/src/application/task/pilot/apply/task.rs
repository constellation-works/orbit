//! One task's assessment checked, completed with the host's findings, admitted
//! under the apply's promotion authority and written atomically.
//!
//! Each phase returns the task's outcome as its error, so the first refusal
//! settles the task and its siblings still apply.
use super::super::PostDeployRequirement;
use super::super::persist::{
    source_superseded, superseded_with_current, task_operation_id, task_outcome,
};
use super::super::recheck::{complete_recheck, mark_full};
use super::admission::{PreparedTaskSnapshot, PromotionAuthority, ValidatedTask};
use super::{Applied, ApplyContext, assess, scope, write};
use crate::application::task::{MachineRequirement, NativeOsRequirement};
use orbit_common::OrbitError;
use orbit_types::task::TaskComplexity;
use serde_json::{Value, json};

/// A task's outcome: `Ok` once its assessment is applied, `Err` with any
/// other settlement.
pub(super) type Settled<T> = Result<T, Value>;

pub(super) fn refused<'a, E: ToString>(
    task_id: &'a str,
    outcome: &'static str,
) -> impl FnOnce(E) -> Value + 'a {
    move |error| task_outcome(task_id, outcome, Some(error.to_string()))
}

/// The selectors an assessment leaves the task with, and what the host kept
/// or flagged on the way.
pub(super) struct Scope {
    pub(super) after: Vec<String>,
    pub(super) normalizations: Vec<Value>,
    /// Granted creation targets the pilot omitted.
    pub(super) retained: Vec<String>,
    /// A review-filed task's evidence the pilot omitted.
    pub(super) evidence: Vec<String>,
    pub(super) reauthorization: Vec<String>,
}

/// The assessment as apply records it, with the host's findings attached.
pub(super) struct Assessed {
    pub(super) assessment: Value,
    pub(super) after: Vec<String>,
    pub(super) complexity: TaskComplexity,
    pub(super) required_os: Vec<NativeOsRequirement>,
    pub(super) required_machine: Option<MachineRequirement>,
    pub(super) post_deploy: Vec<PostDeployRequirement>,
    pub(super) user_facing_ui: bool,
}

pub(super) fn settle_task(
    context: &ApplyContext<'_>,
    applied: &mut Applied,
    partition_index: u64,
    task_id: &String,
    assessment: &Value,
    snapshot: &PreparedTaskSnapshot,
) -> Settled<Value> {
    if let Some(detail) = context.source_superseded.get(task_id) {
        return Err(source_superseded(task_id, detail));
    }
    let ci_sweep = matches!(context.authority, PromotionAuthority::CiSweep(..));
    // One pre-lock read covers the superseded, scope and assess checks below.
    // The locked write re-reads under the task locks, so sharing here changes
    // no decision, only work performed.
    let pre_current = match context.runtime.get_task(task_id) {
        Ok(task) => Some(task),
        Err(OrbitError::NotFound { .. }) => None,
        Err(error) => {
            return Err(task_outcome(
                task_id,
                "apply_failed",
                Some(format!("reload task {task_id}: {error}")),
            ));
        }
    };
    let pre_history = if pre_current.is_some() {
        context
            .runtime
            .get_task_history(task_id)
            .map_err(refused(task_id, "apply_failed"))?
    } else {
        Vec::new()
    };
    if let Some(current) = pre_current.as_ref()
        && let Some(outcome) = superseded_with_current(
            context.runtime,
            current,
            &pre_history,
            snapshot,
            context.policy,
            ci_sweep,
        )
        .map_err(refused(task_id, "apply_failed"))?
    {
        return Err(outcome);
    }
    // A deleted task cannot be superseded (the helper returns None without
    // a task to compare), so it falls through to scope/assess, which settle
    // it stale like before.
    let mut assessment = assessment.clone();
    // A re-check is completed from its prior assessment first, then
    // validated, proven and written exactly as a full assessment is.
    let mode = &snapshot.pilot_mode;
    match &mode.recheck {
        Some(recheck) => complete_recheck(&mut assessment, recheck)
            .map_err(|error| task_outcome(task_id, "invalid", Some(error)))?,
        None => mark_full(
            &mut assessment,
            mode.reason.as_deref(),
            mode.prior_assessment_id.as_deref(),
        ),
    }
    let scope = scope::scope_with(
        context,
        task_id,
        &mut assessment,
        snapshot,
        pre_current.as_ref(),
        &pre_history,
    )?;
    let assessed = assess::assess_with(
        context,
        task_id,
        &assessment,
        snapshot,
        scope,
        pre_current.as_ref(),
    )?;
    // A valid assessment from a lower-effort `@auto` pilot that warrants a
    // higher effort is held back for the repair round's single re-pilot.
    if let Some(outcome) = escalation(context, partition_index, task_id, snapshot, &assessed) {
        return Err(outcome);
    }
    let operation_id = task_operation_id(context.prepared, task_id, &assessed.assessment);
    let validated = ValidatedTask {
        task_id: task_id.clone(),
        after: assessed.after,
        assessment: assessed.assessment,
        admission: None,
        promote: false,
        history_decision: None,
        complexity: assessed.complexity,
        operation_id,
        required_os: assessed.required_os,
        required_machine: assessed.required_machine,
        post_deploy: assessed.post_deploy,
        user_facing_ui: assessed.user_facing_ui,
        applied_material: None,
        pilot_run_id: context.run_id.map(str::to_string),
        pilot_crew: context.partition_crew(partition_index).map(str::to_string),
        pilot_auto_crew: context
            .partition_draws
            .get(&partition_index)
            .map(|draw| draw.crew.clone()),
    };
    write::write(context, applied, validated, snapshot)
}

/// The escalation outcome of `task_id`'s valid assessment, or `None` when it
/// applies as it is: only in the first round, only for a partition whose
/// pilot ran at an `@auto` effort, and only when
/// [`orbit_config::AutoEffortPolicy::pilot_escalation`] names a higher one.
fn escalation(
    context: &ApplyContext<'_>,
    partition_index: u64,
    task_id: &str,
    snapshot: &PreparedTaskSnapshot,
    assessed: &Assessed,
) -> Option<Value> {
    if context.round != super::super::auto_effort::FIRST_ROUND {
        return None;
    }
    let choice = context
        .partition_draws
        .get(&partition_index)?
        .auto_effort
        .as_ref()?;
    let settings = context.runtime.context.settings();
    let crew = settings
        .crews()
        .get(orbit_types::identity::auto_effort_base(&choice.requested)?)?;
    let confidence = assessed.assessment["confidence"].as_str();
    let effort = settings.auto_effort().pilot_escalation(
        crew,
        choice,
        snapshot.complexity,
        Some(assessed.complexity),
        confidence,
    )?;
    let mut outcome = task_outcome(
        task_id,
        super::ESCALATED,
        Some(format!(
            "pilot {} assessed {} with confidence {}; re-piloting once at {}@{effort}",
            choice.resolved,
            assessed.complexity,
            confidence.unwrap_or("unknown"),
            crew.name,
        )),
    );
    outcome[super::super::auto_effort::AUTO_EFFORT_ESCALATION] = json!({
        "effort": effort,
        "from": choice.resolved,
    });
    Some(outcome)
}
