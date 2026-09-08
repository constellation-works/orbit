//! Auto-task scheduling policy with an explicit clock and Core lifecycle adapter.

use super::loader::{AutoTaskLoadError, collect_auto_tasks};
use super::schedule::{AutoTaskDueDecision, decide_due};
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::compose::auto_task::{
    CursorSession, cursor_state_path, load_cursor_state, with_cursor_lock,
};
use orbit_types::workflow::{
    AutoTaskCursor, AutoTaskDefinition, AutoTaskPendingClaim, DedupePolicy,
};
use std::path::PathBuf;

#[cfg(test)]
use std::sync::{Arc, Barrier};

/// Lifecycle adapter. Core retains task creation, authorization and audit.
pub trait AutoTaskDispatch {
    fn evaluate_delivery(
        &self,
        definition: &AutoTaskDefinition,
        dry_run: bool,
        now: DateTime<Utc>,
    ) -> Result<orbit_types::workflow::automation::AutomationDiagnostic, OrbitError>;

    fn definition_root(&self) -> PathBuf;

    fn state_dir(&self) -> PathBuf;

    fn has_open_instance(&self, definition: &AutoTaskDefinition) -> Result<bool, OrbitError>;

    fn mint_task(&self, definition: &AutoTaskDefinition) -> Result<String, OrbitError>;
}

/// Per-definition outcome of one scheduler pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoTaskFireReport {
    /// Definition name.
    pub name: String,
    /// One of: `fired`, `would_fire`, `baselined`, `would_baseline`, `skipped`.
    pub action: &'static str,
    /// Why, for `skipped` rows; for `fired` rows, when the cursor checkpoint
    /// after minting failed or a pending mint was reconciled.
    pub reason: Option<String>,
    /// Scheduled slot consumed (RFC 3339, UTC), when a fire was involved.
    pub slot: Option<String>,
    /// Task minted by a fire.
    pub task_id: Option<String>,
    pub automation: Option<orbit_types::workflow::automation::AutomationDiagnostic>,
}

/// Result of one scheduler pass.
#[derive(Debug, Default)]
pub struct AutoTaskSchedulerOutcome {
    /// Per-definition outcomes.
    pub reports: Vec<AutoTaskFireReport>,
    /// Fail-closed load failures (those definitions were absent this pass).
    pub errors: Vec<AutoTaskLoadError>,
}

/// Options for one scheduler pass.
#[derive(Debug, Clone, Copy, Default)]
pub struct SchedulerOptions {
    /// Report what would fire without recording or creating anything.
    pub dry_run: bool,
}

/// Run one scheduler pass over the definitions in `runtime`'s workspace at an
/// explicit `now` (the test seam). Loads definitions from
/// `<local_orbit_dir>/auto_tasks/`, cursors from
/// `<shared_orbit_dir>/state/auto-tasks.json`.
///
/// Slot admission and cursor writes share a stable sidecar lock. Concurrent
/// passes serialize there; `max_active_runs` on the job is not this guarantee.
pub fn run_auto_task_scheduler_at(
    host: &dyn AutoTaskDispatch,
    now: DateTime<Utc>,
    options: SchedulerOptions,
) -> Result<AutoTaskSchedulerOutcome, OrbitError> {
    // Definitions are tracked checkout content; cursor state is host-local
    // coordination state shared by linked worktrees.
    let definition_root = host.definition_root();
    let state_path = cursor_state_path(&host.state_dir());

    let collection = collect_auto_tasks(&definition_root);

    let mut reports = Vec::new();
    for loaded in &collection.definitions {
        let definition = &loaded.definition;
        let report =
            fire_definition(host, definition, &state_path, now, options).unwrap_or_else(|error| {
                AutoTaskFireReport {
                    name: definition.name.clone(),
                    action: "skipped",
                    reason: Some(format!("error: {error}")),
                    slot: None,
                    task_id: None,
                    automation: None,
                }
            });
        reports.push(report);
    }

    Ok(AutoTaskSchedulerOutcome {
        reports,
        errors: collection.errors,
    })
}

fn fire_definition(
    host: &dyn AutoTaskDispatch,
    definition: &AutoTaskDefinition,
    state_path: &std::path::Path,
    now: DateTime<Utc>,
    options: SchedulerOptions,
) -> Result<AutoTaskFireReport, OrbitError> {
    if matches!(
        definition.schedule,
        orbit_types::workflow::AutoTaskSchedule::Deliveries { .. }
    ) {
        let diagnostic = host.evaluate_delivery(definition, options.dry_run, now)?;
        let task_id = diagnostic
            .state
            .as_ref()
            .and_then(|state| state.active.as_ref())
            .and_then(|attempt| attempt.action_id.clone());

        return Ok(AutoTaskFireReport {
            name: definition.name.clone(),
            action: "delivery",
            reason: Some(diagnostic.reason.clone()),
            slot: None,
            task_id,
            automation: Some(diagnostic),
        });
    }

    if options.dry_run {
        return dry_run_definition(host, definition, state_path, now);
    }

    #[cfg(test)]
    wait_for_admission_overlap();
    with_cursor_lock(state_path, |session| {
        fire_locked(host, definition, session, now)
    })
}

fn dry_run_definition(
    host: &dyn AutoTaskDispatch,
    definition: &AutoTaskDefinition,
    state_path: &std::path::Path,
    now: DateTime<Utc>,
) -> Result<AutoTaskFireReport, OrbitError> {
    if !definition.enabled {
        return Ok(skipped(definition, "disabled"));
    }

    let state = load_cursor_state(state_path)?;
    let Some(cursor) = state.definitions.get(&definition.name) else {
        return Ok(action(definition, "would_baseline"));
    };
    if let Some(pending) = &cursor.pending {
        return Ok(unresolved_pending(definition, pending));
    }

    let baseline = parse_rfc3339(&cursor.baseline_at)?;
    let last_slot = cursor.last_slot.as_deref().map(parse_rfc3339).transpose()?;
    match decide_due(&definition.schedule, baseline, last_slot, now)? {
        AutoTaskDueDecision::NotDue => Ok(skipped(definition, "not_due")),
        AutoTaskDueDecision::Fire { slot } => {
            if definition.dedupe == DedupePolicy::SkipIfOpen
                && host.has_open_instance(definition)?
            {
                return Ok(AutoTaskFireReport {
                    slot: Some(slot),
                    ..skipped(definition, "dedupe_open")
                });
            }
            Ok(AutoTaskFireReport {
                slot: Some(slot),
                ..action(definition, "would_fire")
            })
        }
    }
}

fn fire_locked(
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
                && host.has_open_instance(definition)?
            {
                return Ok(AutoTaskFireReport {
                    slot: Some(slot),
                    ..skipped(definition, "dedupe_open")
                });
            }
            fire_slot(host, definition, session, cursor, slot, now)
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
            Ok(()) => Ok(Some(AutoTaskFireReport {
                name: definition.name.clone(),
                action: "fired",
                reason: Some(format!(
                    "reconciled pending mint for slot {} as {task_id}",
                    pending.slot
                )),
                slot: Some(pending.slot),
                task_id: Some(task_id),
                automation: None,
            })),
            Err(error) => Ok(Some(AutoTaskFireReport {
                name: definition.name.clone(),
                action: "fired",
                reason: Some(format!(
                    "cursor not advanced; retry will reconcile from pending mint evidence or report unresolved: {error}"
                )),
                slot: Some(pending.slot),
                task_id: Some(task_id),
                automation: None,
            })),
        };
    }

    Ok(Some(unresolved_pending(definition, &pending)))
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
        },
    );
    session.save()
}

fn unresolved_pending(
    definition: &AutoTaskDefinition,
    pending: &AutoTaskPendingClaim,
) -> AutoTaskFireReport {
    AutoTaskFireReport {
        slot: Some(pending.slot.clone()),
        ..skipped(
            definition,
            &format!(
                "unresolved_pending: slot {} was claimed without mint evidence; inspect {} tagged tasks and auto-tasks.json before retrying — refusing to remint or consume the slot",
                pending.slot,
                orbit_types::workflow::auto_task_tag(&definition.name)
            ),
        )
    }
}

fn skipped(definition: &AutoTaskDefinition, reason: &str) -> AutoTaskFireReport {
    AutoTaskFireReport {
        name: definition.name.clone(),
        action: "skipped",
        reason: Some(reason.to_string()),
        slot: None,
        task_id: None,
        automation: None,
    }
}

fn action(definition: &AutoTaskDefinition, action: &'static str) -> AutoTaskFireReport {
    AutoTaskFireReport {
        name: definition.name.clone(),
        action,
        reason: None,
        slot: None,
        task_id: None,
        automation: None,
    }
}

fn parse_rfc3339(raw: &str) -> Result<DateTime<Utc>, OrbitError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| OrbitError::Store(format!("invalid stored timestamp '{raw}': {error}")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchedulerFault {
    AfterClaim,
    AfterMintBeforeCheckpoint,
    CheckpointWrite,
}

#[cfg(test)]
thread_local! {
    static INJECTED_FAULT: std::cell::RefCell<Option<SchedulerFault>> =
        const { std::cell::RefCell::new(None) };
    static ADMISSION_BARRIER: std::cell::RefCell<Option<Arc<Barrier>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn inject_scheduler_fault(fault: Option<SchedulerFault>) {
    INJECTED_FAULT.with(|cell| *cell.borrow_mut() = fault);
}

#[cfg(test)]
pub(crate) fn set_admission_overlap_barrier(barrier: Option<Arc<Barrier>>) {
    ADMISSION_BARRIER.with(|cell| *cell.borrow_mut() = barrier);
}

fn fail_if_injected(_fault: SchedulerFault) -> Result<(), OrbitError> {
    #[cfg(test)]
    {
        let hit = INJECTED_FAULT.with(|cell| {
            let current = *cell.borrow();
            if current == Some(_fault) {
                *cell.borrow_mut() = None;
                true
            } else {
                false
            }
        });
        if hit {
            return Err(OrbitError::Store(format!(
                "injected scheduler interruption at {_fault:?}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
fn wait_for_admission_overlap() {
    let barrier = ADMISSION_BARRIER.with(|cell| cell.borrow().clone());
    if let Some(barrier) = barrier {
        barrier.wait();
    }
}
