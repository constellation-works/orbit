//! Auto-task scheduling policy with an explicit clock and Core lifecycle adapter.

use super::loader::{AutoTaskLoadError, collect_auto_tasks, load_definition_file};
use super::schedule::{AutoTaskDueDecision, decide_due};
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_store::compose::auto_task::{
    CursorSession, cursor_state_path, load_cursor_state, with_cursor_lock,
};
use orbit_types::workflow::{
    AutoTaskCursor, AutoTaskDefinition, AutoTaskPendingClaim, AutoTaskSkipRecord, DedupePolicy,
    InactivePluginWarning, SkipIfUnchanged,
};
use std::path::{Path, PathBuf};

/// A plugin-inactive skip with stable provenance for warning deduplication.
pub struct InactivePluginSkip {
    /// Plugin namespace that seeded the definition.
    pub plugin: String,
    /// Seeded plugin version.
    pub version: String,
    /// Operator-facing explanation.
    pub reason: String,
}

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

    /// The id of a still-open instance of `definition`'s prior mints, if any.
    /// `None` means `SkipIfOpen` dedupe should let the current fire proceed.
    fn has_open_instance(
        &self,
        definition: &AutoTaskDefinition,
    ) -> Result<Option<String>, OrbitError>;

    fn mint_task(&self, definition: &AutoTaskDefinition) -> Result<String, OrbitError>;

    /// Why this definition is skipped this pass without being disabled or due.
    ///
    /// The one caller is a definition a plugin seeded whose plugin is no
    /// longer active: it stays on disk, does not fire, and the reason names
    /// the plugin. Hosts without plugins answer `None`.
    fn skip_reason(&self, _definition: &AutoTaskDefinition) -> Option<InactivePluginSkip> {
        None
    }

    /// Evidence for a `skip_if_unchanged` precondition: the tip of the
    /// configured ref, the cursor the last completed sweep recorded, and
    /// whether the tip is already covered by it. Hosts that cannot answer
    /// report [`ChangeProbe::Unknown`] (or an error) and the scheduler mints.
    fn probe_change_since_last_sweep(
        &self,
        definition: &AutoTaskDefinition,
        precondition: &SkipIfUnchanged,
    ) -> Result<ChangeProbe, OrbitError>;
}

/// What a host could establish about the integration branch since the last
/// completed sweep. Fail-open is the scheduler's rule, not the host's: only
/// [`ChangeProbe::Unchanged`] ever suppresses a mint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeProbe {
    /// The ref's tip is already covered by the recorded cursor.
    Unchanged {
        cursor: String,
        tip: String,
        cursor_task_id: Option<String>,
    },
    /// The ref advanced past the recorded cursor.
    Changed { cursor: String, tip: String },
    /// Nothing conclusive — no completed sweep, an unreadable cursor, or git
    /// could not answer. The scheduler mints and records `reason`.
    Unknown { reason: String },
}

/// Machine-readable reason token recorded for a suppressed mint.
pub const UNCHANGED_SINCE_LAST_SWEEP: &str = "unchanged_since_last_sweep";

/// Per-definition outcome of one scheduler pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoTaskFireReport {
    /// Definition name.
    pub name: String,
    /// One of: `fired`, `would_fire`, `baselined`, `would_baseline`, `skipped`,
    /// `error`.
    pub action: &'static str,
    /// Why, for `skipped` rows; for `fired` rows, when the cursor checkpoint
    /// after minting failed or a pending mint was reconciled.
    pub reason: Option<String>,
    /// Scheduled slot consumed (RFC 3339, UTC), when a fire was involved.
    pub slot: Option<String>,
    /// Task minted by a fire.
    pub task_id: Option<String>,
    /// The still-open task id that caused a `dedupe_open` skip.
    pub blocking_task_id: Option<String>,
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
            if let Some(precondition) = &definition.skip_if_unchanged
                && let ChangeProbe::Unchanged {
                    cursor: cursor_sha,
                    tip,
                    cursor_task_id,
                } = probe(host, definition, precondition)
            {
                let reason = skip_reason(&AutoTaskSkipRecord {
                    at: now.to_rfc3339(),
                    slot: slot.clone(),
                    reason: UNCHANGED_SINCE_LAST_SWEEP.to_string(),
                    reference: precondition.reference.clone(),
                    cursor_sha,
                    tip_sha: tip,
                    cursor_task_id,
                });
                return Ok(AutoTaskFireReport {
                    slot: Some(slot),
                    ..skipped(definition, &reason)
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
            if let Some(precondition) = &definition.skip_if_unchanged {
                match probe(host, definition, precondition) {
                    ChangeProbe::Unchanged {
                        cursor: cursor_sha,
                        tip,
                        cursor_task_id,
                    } => {
                        let record = AutoTaskSkipRecord {
                            at: now.to_rfc3339(),
                            slot: slot.clone(),
                            reason: UNCHANGED_SINCE_LAST_SWEEP.to_string(),
                            reference: precondition.reference.clone(),
                            cursor_sha: cursor_sha.clone(),
                            tip_sha: tip.clone(),
                            cursor_task_id: cursor_task_id.clone(),
                        };
                        let reason = skip_reason(&record);
                        // The slot is deliberately left unconsumed: the next
                        // commit past the cursor fires the pending occurrence
                        // immediately, exactly as `dedupe_open` does.
                        session.state.definitions.insert(
                            definition.name.clone(),
                            AutoTaskCursor {
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
                    ChangeProbe::Changed { .. } => {}
                    // Fail open, and say why: an unanswerable precondition
                    // must never be the reason a sweep stops running.
                    ChangeProbe::Unknown { reason } => {
                        let mut report = fire_slot(host, definition, session, cursor, slot, now)?;
                        if report.reason.is_none() {
                            report.reason = Some(format!(
                                "{UNCHANGED_SINCE_LAST_SWEEP} precondition inconclusive; minted: {reason}"
                            ));
                        }
                        return Ok(report);
                    }
                }
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

/// Ask the host, folding a host-side error into the fail-open answer so a
/// broken probe can never stop a definition from minting.
fn probe(
    host: &dyn AutoTaskDispatch,
    definition: &AutoTaskDefinition,
    precondition: &SkipIfUnchanged,
) -> ChangeProbe {
    match host.probe_change_since_last_sweep(definition, precondition) {
        Ok(probe) => probe,
        Err(error) => ChangeProbe::Unknown {
            reason: format!("probe failed: {error}"),
        },
    }
}

/// The audit row's reason: the token plus the two SHAs it was decided on.
fn skip_reason(record: &AutoTaskSkipRecord) -> String {
    format!(
        "{}: {} tip {} is already covered by cursor {}{}",
        record.reason,
        record.reference,
        short_sha(&record.tip_sha),
        short_sha(&record.cursor_sha),
        record
            .cursor_task_id
            .as_ref()
            .map(|id| format!(" from {id}"))
            .unwrap_or_default()
    )
}

fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
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

/// Preview the same recovery outcome a live pass reports after checkpointing.
fn pending_claim_report(
    definition: &AutoTaskDefinition,
    pending: &AutoTaskPendingClaim,
) -> AutoTaskFireReport {
    if let Some(task_id) = &pending.task_id {
        return AutoTaskFireReport {
            reason: Some(format!(
                "reconciled pending mint for slot {} as {task_id}",
                pending.slot
            )),
            slot: Some(pending.slot.clone()),
            task_id: Some(task_id.clone()),
            ..action(definition, "fired")
        };
    }

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
        blocking_task_id: None,
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
        blocking_task_id: None,
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
    static AFTER_LOAD_BARRIERS: std::cell::RefCell<Option<(Arc<Barrier>, Arc<Barrier>)>> =
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

#[cfg(test)]
pub(crate) fn set_after_load_barriers(barriers: Option<(Arc<Barrier>, Arc<Barrier>)>) {
    AFTER_LOAD_BARRIERS.with(|cell| *cell.borrow_mut() = barriers);
}

#[cfg(test)]
fn wait_after_load() {
    let barriers = AFTER_LOAD_BARRIERS.with(|cell| cell.borrow().clone());
    if let Some((loaded, resume)) = barriers {
        loaded.wait();
        resume.wait();
    }
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
