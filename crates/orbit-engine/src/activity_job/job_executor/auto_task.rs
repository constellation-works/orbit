//! The `auto-task:` step evaluator (ORB-20158;
//! `docs/design/activity-job/specs/auto-task-step.md` §2-§5).
//!
//! One pass observes the step through the owner, then settles it, parks it,
//! or reserves and admits its one keyed task. Every record is checkpointed
//! before the engine relies on it: a failed checkpoint stops the pass before
//! any admission. Order per pass: a trusted terminal receipt, then expiry,
//! then an outstanding reservation, which only admission resolves, then a
//! held definition or unmet readiness, then new admission.

use orbit_types::workflow::{AutoTaskReceipt, AutoTaskStepRecord, AutoTaskStepStatus, ParkedWait};

use crate::context::{
    AutoTaskAdmission, AutoTaskObservation, AutoTaskReadiness, AutoTaskStepRequest,
};

use super::*;

/// Run one pass of an `auto-task:` step. A settled step returns its outcome;
/// an unsettled one returns [`DispatchError::AutoTaskParked`].
pub(super) fn run_auto_task_step(
    step: &JobV2Step,
    auto_task: &str,
    ctx: &ExecCtx<'_>,
) -> Result<StepOutcome, DispatchError> {
    let request = AutoTaskStepRequest {
        step_id: step.id.clone(),
        auto_task: auto_task.to_string(),
    };
    let observation = ctx.host.observe_auto_task_step(&ctx.run_id, &request)?;
    let mut record = match observation.record.clone() {
        Some(record) => record,
        None => {
            // Activation is persisted once; the wait clock never resets.
            let record = AutoTaskStepRecord {
                auto_task: auto_task.to_string(),
                activated_at: observation.now,
                action_key: observation.action_key.clone(),
                input_digest: observation.input_digest.clone(),
                status: AutoTaskStepStatus::Waiting,
                reserved_at: None,
                task_id: None,
                minted: false,
                receipt: None,
                settled_at: None,
                diagnostic: None,
            };
            checkpoint(ctx, &step.id, &record)?;
            record
        }
    };
    if record.status.is_settled() {
        return Ok(settled_outcome(step, &record));
    }

    if settle(&mut record, &observation) {
        checkpoint(ctx, &step.id, &record)?;
        return Ok(settled_outcome(step, &record));
    }

    if let Some(task_id) = record.task_id.clone() {
        let status = match observation.bound_status.as_deref() {
            Some("in-progress" | "review") => AutoTaskStepStatus::Running,
            _ => AutoTaskStepStatus::Queued,
        };
        let raw = observation.bound_status.as_deref().unwrap_or("unknown");
        return park(
            ctx,
            &step.id,
            &mut record,
            status,
            format!("waiting for bound task {task_id} ({raw})"),
            &observation,
        );
    }
    // An outstanding reservation may already have created its task, which
    // this pass's board view still counts: resolve it by its creation
    // receipt first. Admission rechecks expiry, the definition and
    // readiness under the fence before any new mint.
    let reserved = record.reserved_at.is_some();
    if !reserved && let Some(hold) = observation.definition_hold.clone() {
        return park(
            ctx,
            &step.id,
            &mut record,
            AutoTaskStepStatus::Held,
            hold,
            &observation,
        );
    }
    if !reserved && let AutoTaskReadiness::Waiting(reason) = &observation.readiness {
        return park(
            ctx,
            &step.id,
            &mut record,
            AutoTaskStepStatus::Waiting,
            format!("wait_until: {reason}"),
            &observation,
        );
    }

    // Reserve durably before any external effect; the owner resolves the
    // reservation by its action key after an ambiguous reply.
    if !reserved {
        record.reserved_at = Some(observation.now);
        record.status = AutoTaskStepStatus::Queued;
        record.diagnostic = None;
        checkpoint(ctx, &step.id, &record)?;
    }
    match ctx.host.admit_auto_task_step(&ctx.run_id, &step.id)? {
        AutoTaskAdmission::Bound { task_id, minted } => {
            let reason = format!(
                "{} task {task_id}; waiting for its accepted completion",
                if minted { "minted" } else { "bound open" }
            );
            record.task_id = Some(task_id);
            record.minted = minted;
            park(
                ctx,
                &step.id,
                &mut record,
                AutoTaskStepStatus::Queued,
                reason,
                &observation,
            )
        }
        AutoTaskAdmission::Waiting(reason) => park(
            ctx,
            &step.id,
            &mut record,
            AutoTaskStepStatus::Waiting,
            format!("wait_until at admission: {reason}"),
            &observation,
        ),
        AutoTaskAdmission::Held(reason) => park(
            ctx,
            &step.id,
            &mut record,
            AutoTaskStepStatus::Held,
            reason,
            &observation,
        ),
        AutoTaskAdmission::Expired(reason) => {
            record.status = AutoTaskStepStatus::Timeout;
            record.settled_at = Some(observation.now);
            record.diagnostic = Some(reason);
            checkpoint(ctx, &step.id, &record)?;
            Ok(settled_outcome(step, &record))
        }
    }
}

/// Settle the step from a trusted receipt, then from expiry. A receipt
/// settled at or before the known deadline discharges the step even when it
/// is first observed later; one settled after it cannot evade expiry.
/// Returns whether the record settled.
fn settle(record: &mut AutoTaskStepRecord, observation: &AutoTaskObservation) -> bool {
    if let Some(receipt) = &observation.receipt
        && record.task_id.as_deref() == Some(receipt.task_id.as_str())
    {
        let in_time = !observation.dynamic_expiry
            && observation
                .deadline
                .is_none_or(|deadline| receipt.settled_at <= deadline);
        let receipt = AutoTaskReceipt {
            observed_at: record
                .receipt
                .as_ref()
                .map_or(receipt.observed_at, |earlier| earlier.observed_at),
            ..receipt.clone()
        };
        let status = match (receipt.accepted, in_time) {
            (true, true) => Some(AutoTaskStepStatus::Done),
            (false, _)
                if receipt.task_status == "archived" || receipt.task_status == "rejected" =>
            {
                Some(AutoTaskStepStatus::Cancelled)
            }
            (false, _) => Some(AutoTaskStepStatus::Failed),
            // Accepted too late: retained as late evidence; expiry decides.
            (true, false) => None,
        };
        let diagnostic = format!(
            "bound task {} ended {}{}",
            receipt.task_id,
            receipt.task_status,
            if receipt.accepted {
                ""
            } else {
                " without an accepted completion"
            }
        );
        record.receipt = Some(receipt);
        if let Some(status) = status {
            record.status = status;
            record.settled_at = Some(observation.now);
            record.diagnostic = Some(diagnostic);
            return true;
        }
    }
    if let Some(reason) = &observation.expired {
        record.status = AutoTaskStepStatus::Timeout;
        record.settled_at = Some(observation.now);
        record.diagnostic = Some(reason.clone());
        return true;
    }
    false
}

fn park(
    ctx: &ExecCtx<'_>,
    step_id: &str,
    record: &mut AutoTaskStepRecord,
    status: AutoTaskStepStatus,
    reason: String,
    observation: &AutoTaskObservation,
) -> Result<StepOutcome, DispatchError> {
    record.status = status;
    record.diagnostic = Some(reason.clone());
    checkpoint(ctx, step_id, record)?;
    Err(DispatchError::AutoTaskParked(Box::new(ParkedWait {
        parked_at: observation.now,
        wake_at: observation.recheck_at,
        reason: format!("step `{step_id}` {}: {reason}", status.as_str()),
    })))
}

fn checkpoint(
    ctx: &ExecCtx<'_>,
    step_id: &str,
    record: &AutoTaskStepRecord,
) -> Result<(), DispatchError> {
    ctx.host
        .checkpoint_auto_task_step(&ctx.run_id, step_id, record)
        .map_err(|error| {
            DispatchError::JobExecution(format!(
                "step `{step_id}`: auto-task checkpoint failed, nothing further was admitted: \
                 {error}"
            ))
        })
}

/// The step outcome a settled record answers. Only an accepted completion
/// succeeds; the output is the binding and receipt, never an agent envelope.
fn settled_outcome(step: &JobV2Step, record: &AutoTaskStepRecord) -> StepOutcome {
    let success = record.status == AutoTaskStepStatus::Done;
    StepOutcome {
        success,
        output: serde_json::json!({
            "auto_task": record.auto_task,
            "status": record.status.as_str(),
            "task_id": record.task_id,
            "minted": record.minted,
            "receipt": record.receipt,
            "end_time": record.settled_at,
        }),
        message: (!success).then(|| {
            format!(
                "step `{}` {}: {}",
                step.id,
                record.status.as_str(),
                record
                    .diagnostic
                    .as_deref()
                    .unwrap_or("no accepted completion")
            )
        }),
    }
}

/// Record a `when: false` auto-task step as `skipped`.
pub(super) fn record_skipped_auto_task_step(
    step: &JobV2Step,
    auto_task: &str,
    ctx: &ExecCtx<'_>,
) -> Result<(), DispatchError> {
    let now = chrono::Utc::now();
    let record = AutoTaskStepRecord {
        auto_task: auto_task.to_string(),
        activated_at: now,
        action_key: String::new(),
        input_digest: String::new(),
        status: AutoTaskStepStatus::Skipped,
        reserved_at: None,
        task_id: None,
        minted: false,
        receipt: None,
        settled_at: Some(now),
        diagnostic: Some("when: false".to_string()),
    };
    checkpoint(ctx, &step.id, &record)
}

/// Merge parked waits from several branches: the earliest wake, every reason.
pub(super) fn merge_parked(parks: Vec<ParkedWait>) -> Option<ParkedWait> {
    let mut parks = parks.into_iter();
    let first = parks.next()?;
    Some(parks.fold(first, |mut merged, park| {
        merged.parked_at = merged.parked_at.max(park.parked_at);
        merged.wake_at = merged.wake_at.min(park.wake_at);
        merged.reason = format!("{}; {}", merged.reason, park.reason);
        merged
    }))
}
