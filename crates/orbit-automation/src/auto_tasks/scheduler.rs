//! Auto-task scheduling policy with an explicit clock and Core lifecycle adapter.
use super::loader::{AutoTaskLoadError, collect_auto_tasks};
use super::schedule::{AutoTaskDueDecision, decide_due};
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::compose::auto_task::{cursor_state_path, load_cursor_state, upsert_cursor};
use orbit_types::workflow::{AutoTaskCursor, AutoTaskDefinition, DedupePolicy};
use std::path::PathBuf;
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
    /// Why, for `skipped` rows; for `fired` rows, only when the cursor
    /// checkpoint after minting failed.
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
pub fn run_auto_task_scheduler_at(
    host: &dyn AutoTaskDispatch,
    now: DateTime<Utc>,
    options: SchedulerOptions,
) -> Result<AutoTaskSchedulerOutcome, OrbitError> {
    // ADR-0286: definitions are tracked checkout content, while cursor state
    // is host-local coordination state shared by linked worktrees.
    let definition_root = host.definition_root();
    let state_path = cursor_state_path(&host.state_dir());

    let collection = collect_auto_tasks(&definition_root);
    let cursors = load_cursor_state(&state_path);

    let mut reports = Vec::new();
    for loaded in &collection.definitions {
        let definition = &loaded.definition;
        let cursor = cursors.definitions.get(&definition.name);
        let report = fire_definition(host, definition, cursor, &state_path, now, options)
            .unwrap_or_else(|error| AutoTaskFireReport {
                name: definition.name.clone(),
                action: "skipped",
                reason: Some(format!("error: {error}")),
                slot: None,
                task_id: None,
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
    cursor: Option<&AutoTaskCursor>,
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
            .and_then(|s| s.active.as_ref())
            .and_then(|a| a.action_id.clone());
        return Ok(AutoTaskFireReport {
            name: definition.name.clone(),
            action: "delivery",
            reason: Some(diagnostic.reason.clone()),
            slot: None,
            task_id,
            automation: Some(diagnostic),
        });
    }
    if !definition.enabled {
        return Ok(skipped(definition, "disabled"));
    }

    // First observation: record the baseline and fire nothing — a definition
    // never mints tasks for slots that predate its registration on this host.
    let Some(cursor) = cursor else {
        if options.dry_run {
            return Ok(action(definition, "would_baseline"));
        }
        upsert_cursor(
            state_path,
            &definition.name,
            AutoTaskCursor {
                baseline_at: now.to_rfc3339(),
                last_slot: None,
                last_fired_at: None,
                last_task_id: None,
            },
        )?;
        return Ok(action(definition, "baselined"));
    };

    let baseline = parse_rfc3339(&cursor.baseline_at)?;
    let last_slot = cursor.last_slot.as_deref().map(parse_rfc3339).transpose()?;

    match decide_due(&definition.schedule, baseline, last_slot, now)? {
        AutoTaskDueDecision::NotDue => Ok(skipped(definition, "not_due")),
        AutoTaskDueDecision::Fire { slot } => {
            // Dedupe: never fire while a prior instance is still open, so a
            // stalled backlog cannot accumulate identical tasks. The cursor is
            // deliberately left unadvanced so the pending occurrence fires
            // (once, collapsed) as soon as the queue drains.
            if definition.dedupe == DedupePolicy::SkipIfOpen
                && host.has_open_instance(definition)?
            {
                return Ok(AutoTaskFireReport {
                    slot: Some(slot),
                    ..skipped(definition, "dedupe_open")
                });
            }

            if options.dry_run {
                return Ok(AutoTaskFireReport {
                    slot: Some(slot),
                    ..action(definition, "would_fire")
                });
            }

            let task = host.mint_task(definition)?;
            // The task exists from here on. A cursor that cannot be written
            // must not turn this into a `skipped` row: the operator would
            // see no fire while the backlog gained a task, and every later
            // pass would mint another for the same slot. Report the fire
            // with the task id and say the checkpoint is missing.
            let reason = upsert_cursor(
                state_path,
                &definition.name,
                AutoTaskCursor {
                    baseline_at: cursor.baseline_at.clone(),
                    last_slot: Some(slot.clone()),
                    last_fired_at: Some(now.to_rfc3339()),
                    last_task_id: Some(task.clone()),
                },
            )
            .err()
            .map(|error| {
                format!("cursor not advanced; the next pass may fire this slot again: {error}")
            });
            Ok(AutoTaskFireReport {
                name: definition.name.clone(),
                action: "fired",
                reason,
                slot: Some(slot),
                task_id: Some(task),
                automation: None,
            })
        }
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
