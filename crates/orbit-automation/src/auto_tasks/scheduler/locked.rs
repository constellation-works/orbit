use super::dispatch::AutoTaskDispatch;
use super::faults::{SchedulerFault, fail_if_injected};
use super::fire::guard_hold;
use super::preconditions::{Precondition, preconditions};
use super::report::{
    AutoTaskFireReport, action, parse_rfc3339, pending_claim_report, skip_reason, skipped,
};
use crate::auto_tasks::schedule::{AutoTaskDueDecision, decide_due};
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::compose::auto_task::CursorSession;
use orbit_types::workflow::{
    AutoTaskCursor, AutoTaskDefinition, AutoTaskPendingClaim, DedupePolicy,
};

pub(super) fn fire_locked(
    host: &dyn AutoTaskDispatch,
    definition: &AutoTaskDefinition,
    session: &mut CursorSession,
    now: DateTime<Utc>,
) -> Result<AutoTaskFireReport, OrbitError> {
    if let Some(report) = recover_pending(definition, session, now)? {
        return Ok(report);
    }

    if !definition.enabled {
        return Ok(skipped(definition, "disabled"));
    }

    let Some(cursor) = session.state.definitions.get(&definition.name).cloned() else {
        session.state.definitions.insert(
            definition.name.clone(),
            AutoTaskCursor {
                baseline_at: now.to_rfc3339(),
                last_slot: None,
                last_fired_at: None,
                last_task_id: None,
                pending: None,
                last_skip: None,
            },
        );
        session.save()?;
        return Ok(action(definition, "baselined"));
    };

    let baseline = parse_rfc3339(&cursor.baseline_at)?;
    let last_slot = cursor.last_slot.as_deref().map(parse_rfc3339).transpose()?;

    match decide_due(&definition.schedule, baseline, last_slot, now)? {
        AutoTaskDueDecision::NotDue => Ok(skipped(definition, "not_due")),
        AutoTaskDueDecision::Fire { slot } => {
            if definition.dedupe == DedupePolicy::SkipIfOpen
                && let Some(blocking_task_id) = host.has_open_instance(definition)?
            {
                return Ok(AutoTaskFireReport {
                    slot: Some(slot),
                    blocking_task_id: Some(blocking_task_id),
                    ..skipped(definition, "dedupe_open")
                });
            }
            if let Some((record, consume)) = guard_hold(host, definition, &slot, now) {
                let reason = skip_reason(&record);
                session.state.definitions.insert(
                    definition.name.clone(),
                    AutoTaskCursor {
                        last_slot: if consume {
                            Some(slot.clone())
                        } else {
                            cursor.last_slot.clone()
                        },
                        last_skip: Some(record),
                        ..cursor
                    },
                );
                session.save()?;
                return Ok(AutoTaskFireReport {
                    slot: Some(slot),
                    ..skipped(definition, &reason)
                });
            }
            match preconditions(host, definition, &slot, now) {
                Precondition::Skip(record) => {
                    let reason = skip_reason(&record);
                    // The slot is deliberately left unconsumed: the next
                    // commit past the cursor, or the next qualifying block,
                    // fires the pending occurrence immediately, exactly as
                    // `dedupe_open` does.
                    session.state.definitions.insert(
                        definition.name.clone(),
                        AutoTaskCursor {
                            last_skip: Some(record),
                            ..cursor
                        },
                    );
                    session.save()?;
                    Ok(AutoTaskFireReport {
                        slot: Some(slot),
                        ..skipped(definition, &reason)
                    })
                }
                Precondition::Proceed => fire_slot(host, definition, session, cursor, slot, now),
                // Fail open, and say why: an unanswerable precondition must
                // never be the reason a sweep stops running.
                Precondition::Inconclusive { token, reason } => {
                    let mut report = fire_slot(host, definition, session, cursor, slot, now)?;
                    if report.reason.is_none() {
                        report.reason = Some(format!(
                            "{token} precondition inconclusive; minted: {reason}"
                        ));
                    }
                    Ok(report)
                }
            }
        }
    }
}

fn recover_pending(
    definition: &AutoTaskDefinition,
    session: &mut CursorSession,
    now: DateTime<Utc>,
) -> Result<Option<AutoTaskFireReport>, OrbitError> {
    let Some(cursor) = session.state.definitions.get(&definition.name).cloned() else {
        return Ok(None);
    };
    let Some(pending) = cursor.pending.clone() else {
        return Ok(None);
    };

    if let Some(task_id) = pending.task_id.clone() {
        return match checkpoint_consumed(session, definition, &cursor, &pending.slot, &task_id, now)
        {
            Ok(()) => Ok(Some(pending_claim_report(definition, &pending))),
            Err(error) => Ok(Some(AutoTaskFireReport {
                name: definition.name.clone(),
                action: "fired",
                reason: Some(format!(
                    "cursor not advanced; retry will reconcile from pending mint evidence or report unresolved: {error}"
                )),
                slot: Some(pending.slot),
                task_id: Some(task_id),
                blocking_task_id: None,
                automation: None,
            })),
        };
    }

    Ok(Some(pending_claim_report(definition, &pending)))
}

fn fire_slot(
    host: &dyn AutoTaskDispatch,
    definition: &AutoTaskDefinition,
    session: &mut CursorSession,
    cursor: AutoTaskCursor,
    slot: String,
    now: DateTime<Utc>,
) -> Result<AutoTaskFireReport, OrbitError> {
    let mut claimed = cursor.clone();
    claimed.pending = Some(AutoTaskPendingClaim {
        slot: slot.clone(),
        task_id: None,
    });
    session
        .state
        .definitions
        .insert(definition.name.clone(), claimed);
    session.save()?;
    fail_if_injected(SchedulerFault::AfterClaim)?;

    let task = match host.mint_task(definition) {
        Ok(task) => task,
        Err(error) => {
            session.state.definitions.insert(
                definition.name.clone(),
                AutoTaskCursor {
                    pending: None,
                    ..cursor
                },
            );
            session.save()?;
            return Ok(AutoTaskFireReport {
                name: definition.name.clone(),
                action: "skipped",
                reason: Some(format!("mint failed; slot not consumed: {error}")),
                slot: Some(slot),
                task_id: None,
                blocking_task_id: None,
                automation: None,
            });
        }
    };

    session.state.definitions.insert(
        definition.name.clone(),
        AutoTaskCursor {
            pending: Some(AutoTaskPendingClaim {
                slot: slot.clone(),
                task_id: Some(task.clone()),
            }),
            ..cursor.clone()
        },
    );
    session.save()?;
    fail_if_injected(SchedulerFault::AfterMintBeforeCheckpoint)?;

    match checkpoint_consumed(session, definition, &cursor, &slot, &task, now) {
        Ok(()) => Ok(AutoTaskFireReport {
            name: definition.name.clone(),
            action: "fired",
            reason: None,
            slot: Some(slot),
            task_id: Some(task),
            blocking_task_id: None,
            automation: None,
        }),
        Err(error) => {
            session.state.definitions.insert(
                definition.name.clone(),
                AutoTaskCursor {
                    pending: Some(AutoTaskPendingClaim {
                        slot: slot.clone(),
                        task_id: Some(task.clone()),
                    }),
                    ..cursor
                },
            );
            let evidence = session
                .save()
                .err()
                .map(|save_error| format!("; mint evidence also unpersisted: {save_error}"));
            Ok(AutoTaskFireReport {
                name: definition.name.clone(),
                action: "fired",
                reason: Some(format!(
                    "cursor not advanced; retry will reconcile from pending mint evidence or report unresolved: {error}{}",
                    evidence.unwrap_or_default()
                )),
                slot: Some(slot),
                task_id: Some(task),
                blocking_task_id: None,
                automation: None,
            })
        }
    }
}

fn checkpoint_consumed(
    session: &mut CursorSession,
    definition: &AutoTaskDefinition,
    cursor: &AutoTaskCursor,
    slot: &str,
    task_id: &str,
    now: DateTime<Utc>,
) -> Result<(), OrbitError> {
    fail_if_injected(SchedulerFault::CheckpointWrite)?;
    session.state.definitions.insert(
        definition.name.clone(),
        AutoTaskCursor {
            baseline_at: cursor.baseline_at.clone(),
            last_slot: Some(slot.to_string()),
            last_fired_at: Some(now.to_rfc3339()),
            last_task_id: Some(task_id.to_string()),
            pending: None,
            // A fire supersedes any earlier precondition skip.
            last_skip: None,
        },
    );
    session.save()
}
