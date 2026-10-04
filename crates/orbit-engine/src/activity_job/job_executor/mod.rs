//! v2 Job DAG executor — the Phase 3 runtime for `JobV2` assets.
//!
//! Interprets a `JobV2` step tree with first-class `parallel:`, `when:`,
//! `retry:`, `fan_out:/fan_in:`, and `loop:` constructs (design §4). This
//! module is purely additive; it shares only the boolean-expression
//! evaluator (`crate::condition::evaluate_bool_expr`) with v1.
//!
//! ## Concurrency
//! Parallel branches and fan-out workers run under `std::thread::scope`
//! (matching v1's DAG scheduler). No tokio, no async.
//!
//! ## Audit
//! Every construct emits §7 envelope events (`step.*`, `fanout.dispatched`,
//! `worker.state`, `fanin.joined`, `loop.iteration.{start,end}`,
//! `loop.did_not_converge`). The retry wrapper emits `step.retry` between
//! attempts and `step.denied` when a denial bypasses retry.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use crate::activity_job::{V2ActivityCatalog, resolve_job_target_refs};
use orbit_common::process::jitter::JitterRng;
use orbit_types::workflow::activity_job::{
    ActivityV2Spec, AgentLoopSpec, BackoffStrategy, BranchOutcome, FanInSpec, FanOutBlock, JobV2,
    JobV2Step, JobV2StepBody, JoinMode, LoopBlock, ParallelBlock, RetrySpec, TargetStep,
    V2AuditEventKind,
};
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::Value;

use crate::condition::evaluate_bool_expr;
use crate::template::{self, TemplateContext};

use super::audit_writer::{V2AuditWriter, WriteError};
use super::crew::{apply_resolved_settings, inject_system_crew_input, resolve_crew_settings};
use super::dispatcher::{
    DispatchError, V2DispatchInput, dispatch_v2_activity,
    dispatch_v2_activity_without_run_id_injection,
};
use crate::context::RuntimeHost;

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

mod audit;
mod concurrency;
mod exec_ctx;
mod fan_out;
mod final_recovery;
mod loop_block;
mod parallel;
mod recovery;
mod step;
mod target;
mod templating;
mod validate;

#[cfg(test)]
mod tests;

use self::audit::*;
use self::concurrency::*;
use self::exec_ctx::*;
use self::fan_out::*;
use self::final_recovery::*;
use self::loop_block::*;
use self::parallel::*;
use self::recovery::*;
use self::step::*;
use self::target::*;
use self::templating::*;

pub use self::validate::{validate_job, validate_job_deterministic_actions};

#[derive(Debug, Clone)]
pub struct JobOutcome {
    pub success: bool,
    pub pipeline: Value,
    pub message: Option<String>,
    /// [ORB-00414] Number of audit-write failures observed during the run.
    /// Non-zero means the audit trail is incomplete (see `degraded_audit`).
    pub audit_failures: u64,
    /// [ORB-00414] True when any audit write failed — retry/recovery/debugging
    /// consumers should treat the trail as incomplete.
    pub degraded_audit: bool,
    /// [ORB-10367] Number of telemetry-persistence failures (invocation
    /// traces) observed during the run. Never affects `success`.
    pub telemetry_failures: u64,
    /// [ORB-10367] True when any telemetry write failed — the run's
    /// invocation/token accounting is incomplete, but its work is not.
    pub degraded_telemetry: bool,
}

#[derive(Debug, Clone)]
struct ResolvedRecoveryActivity {
    name: String,
    spec: ActivityV2Spec,
}

pub fn resolve_job_catalog_refs_for_execution(
    job: &mut JobV2,
    catalog: &V2ActivityCatalog,
) -> Result<(), DispatchError> {
    resolve_job_target_refs(job, catalog)
        .map_err(|err| DispatchError::JobValidation(err.to_string()))
}

/// [ORB-10002] Execute a v2 Job, optionally resuming from a persisted
/// checkpoint state.
///
/// When `resume` is `Some`, top-level steps whose global index is recorded
/// as `success` in `resume.step_states` are skipped (a `step.skipped` audit
/// event is emitted) and their recorded outputs are pre-seeded into the
/// pipeline map so later steps see them through `{{ steps.<id>.output.* }}`
/// templates. Checkpoint granularity is the top-level step: `parallel:` /
/// `fan_out:` / `loop:` blocks re-run as a whole if they did not complete,
/// and a completed one restores every pipeline entry it exposed (nested
/// step outputs and fan-in aliases), not only its own output.
/// In-memory agent sessions are not restorable across processes, so resumed
/// steps that share a session start it fresh.
pub fn execute_job_with_resume(
    job: &JobV2,
    input: Value,
    run_id: &str,
    audit: Arc<V2AuditWriter>,
    host: &dyn RuntimeHost,
    resume: Option<&PipelineState>,
) -> Result<JobOutcome, DispatchError> {
    validate_job(job)?;
    // [ORB-10385] Catalog/runtime skew is caught here, before the first step
    // runs — so a job whose (possibly terminal) activity names an action this
    // binary cannot dispatch never reaches `worktree_setup`'s task admission.
    validate_job_deterministic_actions(job, host)?;

    let base_input = merge_job_input(job.default_input.as_ref(), &input);
    let recovery_activity = match (&job.recovery_activity, &job.resolved_recovery_activity) {
        (Some(name), Some(activity)) => Some(ResolvedRecoveryActivity {
            name: name.clone(),
            spec: activity.spec.clone(),
        }),
        _ => None,
    };
    let failure_activity = match (&job.failure_activity, &job.resolved_failure_activity) {
        (Some(name), Some(activity)) => Some(ResolvedRecoveryActivity {
            name: name.clone(),
            spec: activity.spec.clone(),
        }),
        _ => None,
    };
    let final_recovery_activity = match (
        &job.final_recovery_activity,
        &job.resolved_final_recovery_activity,
    ) {
        (Some(name), Some(activity)) => Some(ResolvedRecoveryActivity {
            name: name.clone(),
            spec: activity.spec.clone(),
        }),
        _ => None,
    };

    let pipeline = seed_pipeline_from_resume(job, resume);
    let preparation_refresh = if let Some(resume) = resume {
        crate::executor::automation::vcs::reconcile_resumed_failure_handoff(
            host, job, run_id, resume, &pipeline,
        )?
    } else {
        None
    };

    let ctx = ExecCtx {
        run_id: run_id.to_string(),
        audit: audit.clone(),
        host,
        input: base_input.clone(),
        pipeline: Arc::new(Mutex::new(pipeline_steps_from_raw(pipeline))),
        recovery_activity,
        failure_activity,
        final_recovery_activity,
        item: None,
        iteration: None,
    };

    // [ORB-13907] Final recovery runs at most once per run: a run resumed
    // from state that recorded it never runs it again.
    let mut final_recovery_spent = resume.is_some_and(|state| state.final_recovery.is_some());
    // After a final-recovery `resume`, every step from the resume point runs
    // again, whatever the seeding checkpoint recorded.
    let mut rerunning = false;
    let mut overall_ok = true;
    let mut overall_message = None;
    let mut index = 0;
    while let Some(step) = job.steps.get(index) {
        let step_index = index as u32;
        let refreshes_preparation = preparation_refresh
            .as_ref()
            .is_some_and(|refresh| refresh.step_index == step_index);
        if !rerunning && step_completed_in_resume(resume, step_index) && !refreshes_preparation {
            emit_job_event_lossy(
                &ctx.audit,
                ctx.task_id(),
                V2AuditEventKind::StepSkipped {
                    step_id: step.id.clone(),
                    reason: format!(
                        "resume: step already completed in checkpointed run (index {step_index})"
                    ),
                },
            );
            index += 1;
            continue;
        }
        // Compound steps write nested entries into the shared pipeline; the
        // pre-step map is what tells them apart from earlier steps' entries.
        let pipeline_before = step_is_compound(step).then(|| ctx.pipeline_snapshot());
        let failure = match run_step(step, &ctx) {
            Ok(outcome) if outcome.success => Ok(outcome),
            Ok(outcome) => {
                let message = outcome
                    .message
                    .unwrap_or_else(|| format!("step `{}` completed with success=false", step.id));
                Err((DispatchError::JobExecution(message.clone()), Some(message)))
            }
            Err(error) => Err((error, None)),
        };
        let mut outcome = match failure {
            Ok(outcome) => outcome,
            Err((error, unsuccessful)) => {
                let verdict = attempt_final_recovery(
                    job,
                    &ctx,
                    index,
                    &error.to_string(),
                    &mut final_recovery_spent,
                );
                if let FinalRecoveryVerdict::Resume(target) = verdict {
                    forget_step_outputs_from(&ctx, job, target);
                    rerunning = true;
                    index = target;
                    continue;
                }
                if verdict.runs_failure_activity() {
                    attempt_failure_activity(step, &ctx, &error);
                }
                match unsuccessful {
                    Some(message) => {
                        overall_ok = false;
                        overall_message = Some(message);
                        break;
                    }
                    None => return Err(error),
                }
            }
        };
        if refreshes_preparation && let Some(refresh) = preparation_refresh.as_ref() {
            refresh.annotate_output(&mut outcome.output)?;
            record_pipeline(&ctx, &step.id, outcome.output.clone());
        }
        let compound_outputs = pipeline_before
            .map(|before| compound_outputs_since(&ctx, step, &before))
            .unwrap_or_default();
        checkpoint_completed_step(
            &ctx,
            step_index,
            &step.id,
            &outcome.output,
            &compound_outputs,
        );
        index += 1;
    }

    Ok(JobOutcome {
        success: overall_ok,
        pipeline: ctx.pipeline_value(),
        message: (!overall_ok).then_some(overall_message).flatten(),
        audit_failures: audit.audit_failure_count(),
        degraded_audit: audit.degraded_audit(),
        telemetry_failures: audit.telemetry_failure_count(),
        degraded_telemetry: audit.degraded_telemetry(),
    })
}

/// [ORB-10002] Seed the executor pipeline map from successful checkpoints so
/// skipped steps' outputs stay visible without exposing failed/timed-out data.
///
/// Per completed step, in step order (so a later step's write wins, as it
/// did in the source run): its own output, a top-level fan-in `collect`
/// alias (always the same value as that output, so it is derived rather
/// than stored), then the nested entries in `compound_outputs`. Checkpoints
/// recorded before `compound_outputs` existed restore the first two only.
fn seed_pipeline_from_resume(
    job: &JobV2,
    resume: Option<&PipelineState>,
) -> HashMap<String, Value> {
    let Some(state) = resume else {
        return HashMap::new();
    };

    let mut seeded = HashMap::new();
    for (index, step) in job.steps.iter().enumerate() {
        let step_index = index as u32;
        if state.step_states.get(&step_index) != Some(&JobRunState::Success) {
            continue;
        }
        let Some(output) = state.step_outputs.get(&step_index) else {
            continue;
        };
        if let Some(alias) = fan_in_alias(step) {
            seeded.insert(alias.to_string(), output.clone());
        }
        seeded.insert(step.id.clone(), output.clone());
        if let Some(nested) = state.compound_outputs.get(&step_index) {
            seeded.extend(
                nested
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
        }
    }
    seeded
}

fn step_is_compound(step: &JobV2Step) -> bool {
    matches!(
        step.body,
        JobV2StepBody::Parallel { .. } | JobV2StepBody::FanOut { .. } | JobV2StepBody::Loop { .. }
    )
}

/// The `collect` alias a fan-out step records next to its own id.
fn fan_in_alias(step: &JobV2Step) -> Option<&str> {
    match &step.body {
        JobV2StepBody::FanOut { fan_in, .. } => fan_in.collect.as_deref(),
        _ => None,
    }
}

/// Pipeline entries a completed compound step wrote besides its own output:
/// every key whose value differs from the pre-step map. The step's own id
/// and a top-level fan-in alias are excluded — the checkpoint output already
/// carries both. A rewrite with an unchanged value is omitted, which resumes
/// identically because the earlier writer's checkpoint restores that value.
///
/// A failed branch tolerated by a successful `any` / `quorum` parallel join
/// keeps whatever it recorded, exactly as the uninterrupted run exposed it:
/// the resumed pipeline mirrors what downstream steps saw, not a filtered
/// view. (Fan-out workers write into their own map and never reach this one.)
fn compound_outputs_since(
    ctx: &ExecCtx<'_>,
    step: &JobV2Step,
    before: &PipelineSteps,
) -> BTreeMap<String, Value> {
    let alias = fan_in_alias(step);
    ctx.pipeline_snapshot()
        .iter()
        .filter(|(key, _)| key.as_str() != step.id && Some(key.as_str()) != alias)
        .filter(|(key, value)| before.get(key.as_str()) != Some(*value))
        .map(|(key, value)| (key.clone(), unwrap_step_output(value)))
        .collect()
}

/// [ORB-10002] True when the resume snapshot records this top-level step as
/// completed successfully; such steps are skipped instead of re-executed.
fn step_completed_in_resume(resume: Option<&PipelineState>, step_index: u32) -> bool {
    resume.is_some_and(|state| state.step_states.get(&step_index) == Some(&JobRunState::Success))
}

/// [ORB-13907] Drop what the steps from `index` on recorded, before a
/// final-recovery `resume` reruns them: their own outputs, fan-in aliases,
/// and nested entries. A rerun step whose `when:` is now false must not leave
/// the previous pass's output visible to later templates.
fn forget_step_outputs_from(ctx: &ExecCtx<'_>, job: &JobV2, index: usize) {
    let mut keys = Vec::new();
    for step in &job.steps[index..] {
        collect_step_keys(step, &mut keys);
    }
    let mut steps = ctx.pipeline.lock().unwrap_or_else(PoisonError::into_inner);
    let steps = Arc::make_mut(&mut steps);
    for key in keys {
        steps.remove(key);
    }
}

fn collect_step_keys<'a>(step: &'a JobV2Step, keys: &mut Vec<&'a str>) {
    keys.push(&step.id);
    match &step.body {
        JobV2StepBody::Parallel { parallel } => {
            for branch in &parallel.branches {
                collect_step_keys(branch, keys);
            }
        }
        JobV2StepBody::FanOut { fan_out, fan_in } => {
            keys.extend(fan_in.collect.as_deref());
            collect_step_keys(&fan_out.worker, keys);
        }
        JobV2StepBody::Loop { loop_ } => {
            for nested in &loop_.steps {
                collect_step_keys(nested, keys);
            }
        }
        JobV2StepBody::Target(_) | JobV2StepBody::TargetRef(_) => {}
    }
}

/// [ORB-10002] Persist a checkpoint for a completed top-level step through
/// the host. The payload is what this step exposed — its output plus any
/// compound entries; the host accumulates by step, so persisted bytes per
/// checkpoint stay O(step output). Non-fatal: a checkpoint write failure
/// degrades resumability but must never fail an otherwise-successful run.
fn checkpoint_completed_step(
    ctx: &ExecCtx<'_>,
    step_index: u32,
    step_id: &str,
    output: &Value,
    compound_outputs: &BTreeMap<String, Value>,
) {
    if let Err(error) =
        ctx.host
            .checkpoint_step(&ctx.run_id, step_index, step_id, output, compound_outputs)
    {
        tracing::warn!(
            target: "orbit.engine.job_executor",
            run_id = %ctx.run_id,
            step_id,
            step_index,
            error = %error,
            "step checkpoint persistence failed; run continues without a durable checkpoint",
        );
    }
}
