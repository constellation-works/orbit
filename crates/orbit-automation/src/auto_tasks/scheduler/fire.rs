use super::dispatch::AutoTaskDispatch;
#[cfg(test)]
use super::faults::{wait_after_load, wait_for_admission_overlap};
use super::locked::fire_locked;
use super::preconditions::{Precondition, preconditions};
use super::report::{
    AutoTaskFireReport, AutoTaskSchedulerOutcome, SchedulerOptions, action, parse_rfc3339,
    pending_claim_report, skip_reason, skipped,
};
use crate::auto_tasks::loader::{collect_auto_tasks, load_definition_file};
use crate::auto_tasks::schedule::{AutoTaskDueDecision, decide_due};
use crate::condition::GuardVerdict;
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_store::compose::auto_task::{cursor_state_path, load_cursor_state, with_cursor_lock};
use orbit_types::workflow::automation::ConditionGuardMiss;
use orbit_types::workflow::{
    AutoTaskDefinition, AutoTaskSchedule, AutoTaskSkipRecord, DedupePolicy, InactivePluginWarning,
};
use std::path::Path;

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
    // Definitions belong to this host's registered checkout; cursor state is
    // host-local coordination state shared by linked worktrees.
    let definition_root = host.definition_root();
    let state_path = cursor_state_path(&host.state_dir());

    let collection = collect_auto_tasks(&definition_root);
    #[cfg(test)]
    wait_after_load();

    let mut reports = Vec::new();
    for loaded in &collection.definitions {
        let definition = &loaded.definition;
        let report = fire_definition(host, definition, &loaded.path, &state_path, now, options)
            .unwrap_or_else(|error| AutoTaskFireReport {
                name: definition.name.clone(),
                action: "error",
                reason: Some(error.to_string()),
                slot: None,
                task_id: None,
                blocking_task_id: None,
                automation: None,
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
    definition_path: &Path,
    state_path: &std::path::Path,
    now: DateTime<Utc>,
    options: SchedulerOptions,
) -> Result<AutoTaskFireReport, OrbitError> {
    // A plugin-seeded definition whose plugin is inactive cannot admit work.
    if let Some(skip) = host.skip_reason(definition) {
        let warning = InactivePluginWarning {
            definition: definition.name.clone(),
            plugin: skip.plugin,
            version: skip.version,
        };
        if first_inactive_plugin_warning(state_path, warning, options.dry_run) {
            tracing::warn!(
                target: "orbit.automation.auto_tasks",
                auto_task = %definition.name,
                reason = %skip.reason,
                "skipping auto-task definition",
            );
        } else {
            tracing::debug!(
                target: "orbit.automation.auto_tasks",
                auto_task = %definition.name,
                reason = %skip.reason,
                "skipping auto-task definition",
            );
        }
        return Ok(skipped(definition, &skip.reason));
    }

    // Dry runs leave no cursor state or lock file behind. They perform no
    // admission, so the locked boundary below is only needed for live passes.
    if options.dry_run {
        if let Some(report) = source_skip(definition, definition_path)? {
            return Ok(report);
        }
        if matches!(definition.schedule, AutoTaskSchedule::Condition { .. }) {
            return fire_condition(host, definition, true, now);
        }
        if matches!(
            definition.schedule,
            orbit_types::workflow::AutoTaskSchedule::Deliveries { .. }
        ) {
            return fire_delivery(host, definition, true, now);
        }
        return dry_run_definition(host, definition, state_path, now);
    }

    #[cfg(test)]
    wait_for_admission_overlap();
    // Discovery happens before admission. A delete, edit, or toggle may have
    // replaced this definition while the pass waited for the shared cursor
    // lock, so admission revalidates the loaded revision under that lock.
    // A condition keeps its edge state in the automation store and mints
    // under each edge's key, never through the slot cursor. It shares the
    // cursor file's lock with deletion and edits, so admission acts only on
    // the revision discovery loaded.
    if matches!(definition.schedule, AutoTaskSchedule::Condition { .. }) {
        return with_exclusive_file_lock(state_path, "auto-task cursor", || {
            if let Some(report) = admission_skip(definition, definition_path)? {
                return Ok(report);
            }
            fire_condition(host, definition, false, now)
        });
    }
    if matches!(
        definition.schedule,
        orbit_types::workflow::AutoTaskSchedule::Deliveries { .. }
    ) {
        // Delivery consumers share deletion's lock but do not use cursor
        // contents. A malformed cursor must not stop an unrelated delivery.
        return with_exclusive_file_lock(state_path, "auto-task cursor", || {
            if let Some(report) = admission_skip(definition, definition_path)? {
                return Ok(report);
            }
            fire_delivery(host, definition, false, now)
        });
    }
    with_cursor_lock(state_path, |session| {
        if let Some(report) = admission_skip(definition, definition_path)? {
            return Ok(report);
        }
        fire_locked(host, definition, session, now)
    })
}

/// Whether this workspace has not yet warned about `warning`. The record
/// lives in the cursor state file because every clock tick is a new process.
/// A dry run records nothing. State that cannot be read or written warns
/// rather than hiding the skip.
fn first_inactive_plugin_warning(
    state_path: &Path,
    warning: InactivePluginWarning,
    dry_run: bool,
) -> bool {
    let recorded = load_cursor_state(state_path)
        .is_ok_and(|state| state.inactive_plugin_warnings.contains(&warning));
    if recorded {
        return false;
    }
    if dry_run {
        return true;
    }
    // Recheck under the lock so overlapping ticks warn once between them.
    with_cursor_lock(state_path, |session| {
        if !session.state.inactive_plugin_warnings.insert(warning) {
            return Ok(false);
        }
        session.save()?;
        Ok(true)
    })
    .unwrap_or(true)
}

/// Admission acts only on the revision discovery loaded. Deletion and the
/// CRUD edit and toggle paths commit under the same lock, so a file that no
/// longer parses to that revision here changed after load: the pass skips it
/// and the next pass admits whatever is current.
fn admission_skip(
    definition: &AutoTaskDefinition,
    definition_path: &Path,
) -> Result<Option<AutoTaskFireReport>, OrbitError> {
    if let Some(report) = source_skip(definition, definition_path)? {
        return Ok(Some(report));
    }
    match load_definition_file(definition_path) {
        Ok(current) if current.definition == *definition => Ok(None),
        _ => Ok(Some(skipped(definition, "definition_changed"))),
    }
}

fn source_skip(
    definition: &AutoTaskDefinition,
    definition_path: &Path,
) -> Result<Option<AutoTaskFireReport>, OrbitError> {
    match std::fs::symlink_metadata(definition_path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(None),
        Ok(_) => Ok(Some(skipped(definition, "definition_unavailable"))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(Some(skipped(definition, "definition_removed")))
        }
        Err(error) => Err(OrbitError::Io(format!(
            "inspect auto-task definition {}: {error}",
            definition_path.display()
        ))),
    }
}

fn fire_delivery(
    host: &dyn AutoTaskDispatch,
    definition: &AutoTaskDefinition,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Result<AutoTaskFireReport, OrbitError> {
    let diagnostic = host.evaluate_delivery(definition, dry_run, now)?;
    let task_id = diagnostic
        .state
        .as_ref()
        .and_then(|state| state.active.as_ref())
        .and_then(|attempt| attempt.action_id.clone());

    Ok(AutoTaskFireReport {
        name: definition.name.clone(),
        action: "delivery",
        reason: Some(diagnostic.reason.clone()),
        slot: None,
        task_id,
        blocking_task_id: None,
        automation: Some(diagnostic),
    })
}

fn fire_condition(
    host: &dyn AutoTaskDispatch,
    definition: &AutoTaskDefinition,
    dry_run: bool,
    now: DateTime<Utc>,
) -> Result<AutoTaskFireReport, OrbitError> {
    let diagnostic = host.evaluate_condition(definition, dry_run, now)?;
    let task_id = diagnostic
        .actions
        .iter()
        .rev()
        .find_map(|action| action.action_id.clone());
    let mut reason = diagnostic.reason.clone();
    if !diagnostic.would_fire.is_empty() {
        reason = format!("would_fire: {}", diagnostic.would_fire.join(", "));
    }
    Ok(AutoTaskFireReport {
        name: definition.name.clone(),
        action: "condition",
        reason: Some(reason),
        slot: None,
        task_id,
        blocking_task_id: None,
        automation: None,
    })
}

/// What the schedule's `when:` guard does to the due `slot`: `None` lets it
/// fire. A false or unknown guard leaves the slot unconsumed, so it fires on
/// the first later pass where the guard holds, unless `guard_miss: skip`
/// consumes it (§11).
pub(super) fn guard_hold(
    host: &dyn AutoTaskDispatch,
    definition: &AutoTaskDefinition,
    slot: &str,
    now: DateTime<Utc>,
) -> Option<(AutoTaskSkipRecord, bool)> {
    let guard = definition.schedule.guard()?;
    guard.when.as_ref()?;
    let GuardVerdict::Closed { reason, detail } = host.evaluate_guard(definition, now) else {
        return None;
    };
    let consume = guard.guard_miss == Some(ConditionGuardMiss::Skip);
    Some((
        AutoTaskSkipRecord {
            at: now.to_rfc3339(),
            slot: slot.to_string(),
            reason: reason.to_string(),
            reference: String::new(),
            cursor_sha: String::new(),
            tip_sha: String::new(),
            cursor_task_id: None,
            detail: Some(if consume {
                format!("{detail}; slot consumed (guard_miss: skip)")
            } else {
                detail
            }),
        },
        consume,
    ))
}

fn dry_run_definition(
    host: &dyn AutoTaskDispatch,
    definition: &AutoTaskDefinition,
    state_path: &Path,
    now: DateTime<Utc>,
) -> Result<AutoTaskFireReport, OrbitError> {
    let state = load_cursor_state(state_path)?;
    if let Some(pending) = state
        .definitions
        .get(&definition.name)
        .and_then(|cursor| cursor.pending.as_ref())
    {
        return Ok(pending_claim_report(definition, pending));
    }

    if !definition.enabled {
        return Ok(skipped(definition, "disabled"));
    }

    let Some(cursor) = state.definitions.get(&definition.name) else {
        return Ok(action(definition, "would_baseline"));
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
            if let Some((record, _)) = guard_hold(host, definition, &slot, now) {
                return Ok(AutoTaskFireReport {
                    slot: Some(slot),
                    ..skipped(definition, &skip_reason(&record))
                });
            }
            if let Precondition::Skip(record) = preconditions(host, definition, &slot, now) {
                return Ok(AutoTaskFireReport {
                    slot: Some(slot),
                    ..skipped(definition, &skip_reason(&record))
                });
            }
            Ok(AutoTaskFireReport {
                slot: Some(slot),
                ..action(definition, "would_fire")
            })
        }
    }
}
