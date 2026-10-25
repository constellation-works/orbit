//! Read-only status projection for `orbit routine list` / `show` [ORB-10021]:
//! every routine with both toggle layers (versioned `enabled` and host-local
//! pause), the computed next-due slot, and the last recorded fire — so "why
//! didn't this fire?" is one command. The row, fire and report projections
//! and the toggle selection here serve the CLI, dashboard and MCP alike.

use std::collections::HashSet;
use std::path::Path;

use chrono::{DateTime, Local, Utc};
use orbit_automation::routines::sweep::duplicate_ownership_skipped;
use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_store::contracts::{JobRunQuery, RoutineFireRecord, RoutineFireState};
use orbit_types::workflow::{JobRun, JobRunState, RoutineTarget};
use orbit_types::workspace::{Workspace, WorkspaceStatus};
use serde_json::{Map, Value, json};

use super::RoutineMachineIdentity;
use super::due::{next_occurrence, parse_cron};
use super::loader::{
    DiscoveredWorkspaces, LoadedRoutine, OwnerOnlyRoutine, RetiredRoutine, RoutineLoadError,
    RoutineWorkspaceProvider, collect_host_routines,
};
use crate::OrbitRuntime;

// Bound status reads independently of the compact ten-fire dashboard strip.
const FIRE_HISTORY_LIMIT: usize = 100;
const RECENT_FIRE_LIMIT: usize = 10;

/// Consecutive failures at the head of a routine's fire history.
#[derive(Debug, Clone)]
pub struct RoutineFailureStreak {
    /// Number of distinct fires whose latest attempt failed.
    pub count: usize,
    /// Slot of the oldest failure in the streak (RFC 3339).
    pub since: Option<String>,
    /// History ended before a non-failure was seen: count is a lower bound
    /// and the streak may have started before `since`.
    pub truncated: bool,
}

/// Operator-facing schedule readiness. Theoretical next-slot math may still be
/// present; this state says whether that time is armed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleDisplayState {
    /// Enabled, not paused, and a next slot is a real scheduled evaluation.
    Scheduled,
    /// Definition `enabled` is false. A next slot, if present, is hypothetical.
    Disabled,
    /// Host-local pause. A next slot, if present, is hypothetical.
    Paused,
    /// Enabled delivery- or state-triggered work is waiting on that trigger.
    Waiting,
    /// The scheduler has never recorded a cursor for this definition.
    NeverObserved,
    /// Source or trigger state cannot be shown as a next evaluation, or this
    /// host can never admit the delivery work it would evaluate.
    Unavailable,
}

impl ScheduleDisplayState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Disabled => "disabled",
            Self::Paused => "paused",
            Self::Waiting => "waiting",
            Self::NeverObserved => "never_observed",
            Self::Unavailable => "unavailable",
        }
    }

    /// Disabled and paused rows may still carry a theoretical next slot.
    pub fn is_hypothetical(self) -> bool {
        matches!(self, Self::Disabled | Self::Paused)
    }

    /// The `{state, at, hypothetical}` next-evaluation record. A waiting
    /// trigger has no slot to show.
    pub fn next_evaluation(self, at: Option<String>) -> Value {
        json!({
            "state": self.as_str(),
            "at": if self == Self::Waiting { None } else { at },
            "hypothetical": self.is_hypothetical(),
        })
    }
}

/// Full effective state of one routine on this host.
#[derive(Debug, Clone)]
pub struct RoutineStatus {
    /// The loaded definition plus its provenance.
    pub routine: LoadedRoutine,
    /// Host-local pause, when one is set (RFC 3339 pause timestamp).
    pub paused_at: Option<String>,
    /// First scheduler observation on this host (RFC 3339).
    pub first_observed_at: Option<String>,
    /// Most recent scheduled slot consumed by the scheduler (RFC 3339).
    pub last_evaluated_slot: Option<String>,
    /// Next scheduled slot (RFC 3339, host-local), when computable.
    pub next_due: Option<String>,
    /// Most recent fire recorded on this host, from whichever trigger fired
    /// it: the cron fire store, or for a state- or delivery-triggered routine
    /// the newest run that routine's automation admitted when that is newer.
    pub last_fire: Option<RoutineFireRecord>,
    /// Recent distinct fires, newest first, including automation runs.
    pub recent_fires: Vec<RoutineFireRecord>,
    /// Latest consecutive failures; success, skips and in-flight fires reset it.
    pub failure_streak: RoutineFailureStreak,
    pub automation: Option<serde_json::Value>,
}

impl RoutineStatus {
    /// Whether the routine would currently fire on this host when due:
    /// enabled and not paused.
    pub fn effective(&self) -> bool {
        self.routine.definition.enabled && self.paused_at.is_none()
    }

    /// How Operations (and other projections) should label the next slot.
    pub fn schedule_display_state(&self) -> ScheduleDisplayState {
        if !self.routine.definition.enabled {
            return ScheduleDisplayState::Disabled;
        }
        if self.paused_at.is_some() {
            return ScheduleDisplayState::Paused;
        }
        if automation_unavailable(self.automation.as_ref()) {
            return ScheduleDisplayState::Unavailable;
        }
        if self.routine.definition.trigger.deliveries_landed.is_some()
            || self.routine.definition.trigger.state.is_some()
        {
            return ScheduleDisplayState::Waiting;
        }
        if self.next_due.is_some() {
            return ScheduleDisplayState::Scheduled;
        }
        if self.first_observed_at.is_none() {
            return ScheduleDisplayState::NeverObserved;
        }
        ScheduleDisplayState::Unavailable
    }

    /// This status as the `(key, field)` columns a surface renders, in order.
    pub fn project(&self, columns: &[(&str, RoutineField)]) -> Map<String, Value> {
        columns
            .iter()
            .map(|(key, field)| ((*key).to_string(), self.field(*field)))
            .collect()
    }

    fn field(&self, field: RoutineField) -> Value {
        let routine = &self.routine;
        let definition = &routine.definition;
        match field {
            RoutineField::Name => json!(definition.name),
            RoutineField::Description => json!(definition.description),
            RoutineField::Source => json!(routine.source_workspace),
            RoutineField::Origin => json!(routine.origin.as_str()),
            RoutineField::Path => json!(routine.path.display().to_string()),
            RoutineField::Target => json!(definition.target.as_ref_string()),
            RoutineField::Enabled => json!(definition.enabled),
            RoutineField::PausedAt => json!(self.paused_at),
            RoutineField::Effective => json!(self.effective()),
            RoutineField::Cron => json!(definition.trigger.cron),
            RoutineField::Trigger => json!(definition.trigger),
            RoutineField::MissedRun => json!(definition.trigger.missed_run),
            RoutineField::Policy => json!(definition.policy),
            RoutineField::Automation => json!(self.automation),
            RoutineField::FirstObservedAt => json!(self.first_observed_at),
            RoutineField::LastEvaluatedSlot => json!(self.last_evaluated_slot),
            RoutineField::NextDue => json!(self.next_due),
            RoutineField::NextEvaluation => self
                .schedule_display_state()
                .next_evaluation(self.next_due.clone()),
            RoutineField::State => json!(self.schedule_display_state().as_str()),
            RoutineField::LastFire(columns) => json!(
                self.last_fire
                    .as_ref()
                    .map(|fire| project_fire(fire, columns))
            ),
            RoutineField::RecentFires(columns) => json!(
                self.recent_fires
                    .iter()
                    .map(|fire| project_fire(fire, columns))
                    .collect::<Vec<_>>()
            ),
            RoutineField::FailureStreak => json!({
                "count": self.failure_streak.count,
                "since": self.failure_streak.since,
                "truncated": self.failure_streak.truncated,
            }),
        }
    }
}

/// One field of the shared routine status projection ([`RoutineStatus::project`]).
/// Each surface pairs a field with the key it renders it under.
#[derive(Debug, Clone, Copy)]
pub enum RoutineField {
    Name,
    Description,
    /// Source workspace name.
    Source,
    Origin,
    /// Definition file path.
    Path,
    Target,
    /// Versioned definition `enabled`.
    Enabled,
    PausedAt,
    /// Enabled and not paused.
    Effective,
    Cron,
    /// The whole trigger definition.
    Trigger,
    MissedRun,
    Policy,
    /// Raw automation diagnostic of a state- or delivery-triggered routine.
    Automation,
    FirstObservedAt,
    LastEvaluatedSlot,
    NextDue,
    /// [`ScheduleDisplayState::next_evaluation`] over the next slot.
    NextEvaluation,
    /// [`ScheduleDisplayState`] token.
    State,
    /// The last fire, projected with these columns.
    LastFire(&'static [(&'static str, FireField)]),
    /// Recent distinct fires, newest first, projected with these columns.
    RecentFires(&'static [(&'static str, FireField)]),
    /// `{count, since, truncated}` of the latest consecutive failures.
    FailureStreak,
}

/// One field of the shared fire projection ([`project_fire`]).
#[derive(Debug, Clone, Copy)]
pub enum FireField {
    Slot,
    Attempt,
    State,
    /// Coarse outcome: true on success, false on failure, null otherwise.
    Ok,
    RunId,
    Detail,
    /// When the attempt was recorded.
    StartedAt,
    /// Last update of a terminal attempt; null while it is in flight.
    FinishedAt,
    /// Wall-clock duration of a terminal attempt.
    DurationMs,
    UpdatedAt,
}

/// One fire attempt as the `(key, field)` columns a surface renders, in order.
pub fn project_fire(fire: &RoutineFireRecord, columns: &[(&str, FireField)]) -> Value {
    let terminal = fire.state.is_terminal();
    let field = |field: FireField| match field {
        FireField::Slot => json!(fire.slot),
        FireField::Attempt => json!(fire.attempt),
        FireField::State => json!(fire.state.as_str()),
        FireField::Ok => json!(match fire.state {
            RoutineFireState::Succeeded => Some(true),
            RoutineFireState::Failed | RoutineFireState::TimedOut | RoutineFireState::Error => {
                Some(false)
            }
            RoutineFireState::Skipped | RoutineFireState::Intent | RoutineFireState::Dispatched => {
                None
            }
        }),
        FireField::RunId => json!(fire.run_id),
        FireField::Detail => json!(fire.detail),
        FireField::StartedAt => json!(fire.created_at),
        FireField::FinishedAt => json!(terminal.then(|| fire.updated_at.clone())),
        FireField::DurationMs => json!(terminal.then(|| fire_duration_ms(fire)).flatten()),
        FireField::UpdatedAt => json!(fire.updated_at),
    };
    Value::Object(
        columns
            .iter()
            .map(|(key, column)| ((*key).to_string(), field(*column)))
            .collect(),
    )
}

fn fire_duration_ms(fire: &RoutineFireRecord) -> Option<i64> {
    let start = DateTime::parse_from_rfc3339(&fire.created_at).ok()?;
    let end = DateTime::parse_from_rfc3339(&fire.updated_at).ok()?;
    Some((end - start).num_milliseconds())
}

fn automation_unavailable(automation: Option<&serde_json::Value>) -> bool {
    automation
        .and_then(|value| value.get("reason"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|reason| reason == "source_unavailable" || reason == "state_unavailable")
}

/// Everything `orbit routine list` renders.
#[derive(Debug)]
pub struct RoutineStatusReport {
    /// This machine's display name (`machine.name`).
    pub machine_name: String,
    /// This machine's stable identity (`machine.id`).
    pub machine_id: String,
    /// Per-routine status rows, in discovery order.
    pub statuses: Vec<RoutineStatus>,
    /// Definitions targeting a retired job, or seeded by a plugin that is not
    /// active where they live ([`RetiredRoutine::skipped`]): never scheduled.
    /// List surfaces go through [`Self::listed_retired`].
    pub retired: Vec<RetiredRoutine>,
    /// Fail-closed load failures (these routines are absent).
    pub load_errors: Vec<RoutineLoadError>,
    /// Definitions in a replica checkout that only the owner schedules:
    /// listed with the owner-authority reason, never toggled or fired here.
    pub owner_only: Vec<OwnerOnlyRoutine>,
}

impl RoutineStatusReport {
    /// The unscheduled definitions a list surface shows: every retired one,
    /// and a plugin-parked one only when `include_inactive_plugins` asks.
    pub fn listed_retired(
        &self,
        include_inactive_plugins: bool,
    ) -> impl Iterator<Item = &RetiredRoutine> {
        self.retired.iter().filter(move |routine| {
            crate::application::plugin::is_listed(routine.skipped, include_inactive_plugins)
        })
    }

    /// The plugin-parked definitions a default listing hides.
    pub fn inactive_plugin_routines(&self) -> impl Iterator<Item = &RetiredRoutine> {
        self.retired.iter().filter(|routine| routine.skipped)
    }

    /// The `retired` rows of a routine listing ([`Self::listed_retired`]).
    pub fn retired_rows(&self, include_inactive_plugins: bool) -> Vec<Value> {
        self.listed_retired(include_inactive_plugins)
            .map(|routine| {
                json!({
                    "name": routine.name,
                    "source": routine.source_workspace,
                    "origin": routine.origin.as_str(),
                    "path": routine.path.display().to_string(),
                    "target": format!("job:{}", routine.job),
                    "reason": routine.reason,
                    "plugin_inactive": routine.skipped,
                })
            })
            .collect()
    }

    /// The `owner_only` rows of a routine listing.
    pub fn owner_only_rows(&self) -> Vec<Value> {
        self.owner_only
            .iter()
            .map(|owned| {
                json!({
                    "name": owned.routine.definition.name,
                    "source": owned.routine.source_workspace,
                    "origin": owned.routine.origin.as_str(),
                    "path": owned.routine.path.display().to_string(),
                    "target": owned.routine.definition.target.as_ref_string(),
                    "enabled": owned.routine.definition.enabled,
                    "owner_machine": owned.owner_machine,
                    "reason": owned.reason,
                })
            })
            .collect()
    }

    /// The `load_errors` rows of a routine listing.
    pub fn load_error_rows(&self) -> Vec<Value> {
        self.load_errors
            .iter()
            .map(|error| {
                json!({
                    "source_workspace": error.source_workspace,
                    "path": error.path.as_ref().map(|path| path.display().to_string()),
                    "message": error.message,
                })
            })
            .collect()
    }
}

/// Collect routine status from a caller-supplied workspace provider. Registry
/// ownership remains outside Core.
pub fn routine_statuses_with_providers(
    global_root: &Path,
    local_machine: RoutineMachineIdentity,
    workspace_provider: &dyn RoutineWorkspaceProvider,
    now_utc: DateTime<Utc>,
) -> Result<RoutineStatusReport, OrbitError> {
    let started = std::time::Instant::now();
    let database =
        orbit_config::resolved_audit_db_path(&orbit_config::ConfigRoots::global_only(global_root))?;
    // Existing status reads do not need migration/preflight writes. Preserve
    // first-use initialization on a host that has no scheduler database yet.
    let store: std::sync::Arc<dyn orbit_store::contracts::RoutineStoreBackend> =
        if database.exists() {
            std::sync::Arc::new(orbit_store::Store::open_read_only(&database)?)
        } else {
            super::open_routine_store(global_root)?
        };
    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis(),
        "routine status store opened"
    );

    let discovered = workspace_provider.discover_workspaces(global_root)?;
    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis(),
        "routine status workspaces discovered"
    );
    let mut load_errors = discovered.errors.clone();
    let host = collect_host_routines(&discovered);
    let mut collection = host.collection;
    load_errors.append(&mut collection.errors);

    let pauses = store.routine_pauses()?;
    // One query each for every routine's cursor and fire history, instead of
    // two store reads per routine on the dashboard's poll path.
    let routine_names: Vec<String> = collection
        .routines
        .iter()
        .map(|routine| routine.definition.name.clone())
        .collect();
    let cursors = store.routine_cursors_for(&routine_names)?;
    let fires_by_routine = store.routine_recent_fires_for(&routine_names, FIRE_HISTORY_LIMIT)?;
    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis(),
        "routine status fire history and cursors loaded"
    );
    let now = now_utc.with_timezone(&Local);

    let mut statuses = Vec::with_capacity(collection.routines.len());
    let source_cache = crate::application::automation::SourceCache::default();
    // The sweep skips a duplicate owner before evaluating it, so its status says so.
    let duplicate_owners: Vec<bool> = collection
        .routines
        .iter()
        .map(|routine| duplicate_ownership_skipped(&collection.routines, routine))
        .collect();
    for (routine, duplicate_owner) in collection.routines.into_iter().zip(duplicate_owners) {
        let next_due = next_scheduled_occurrence(&routine.definition.trigger.cron, &now);
        // Keyed by name alone, as the per-routine reads were: duplicate
        // definitions of one name share its cursor and fire history.
        let cron_fires = fires_by_routine
            .get(&routine.definition.name)
            .cloned()
            .unwrap_or_default();
        let cursor = cursors.get(&routine.definition.name).cloned();
        let paused_at = pauses
            .get(&routine.definition.name)
            .map(|pause| pause.paused_at.clone());
        let automation = (routine.definition.trigger.deliveries_landed.is_some()
            || routine.definition.trigger.state.is_some())
        .then(|| {
            discovered
                .entries
                .iter()
                .find(|(_, runtime)| runtime.shared_root() == routine.source_orbit_dir)
                .map_or_else(
                    || serde_json::json!({"reason": "source_unavailable"}),
                    |(_, runtime)| {
                        match crate::application::automation::inspect_routine_with_cache(
                            runtime,
                            &routine.definition,
                            now_utc,
                            &source_cache,
                        ) {
                            Ok(mut value) => {
                                if duplicate_owner {
                                    // The sweep records this skip without admitting any batch.
                                    value.reason = "duplicate_routine_ownership".into();
                                    value.batch.clear();
                                }
                                serde_json::json!(value)
                            }
                            Err(error) => serde_json::json!({
                                "reason": "state_unavailable",
                                "error": error.to_string(),
                            }),
                        }
                    },
                )
        });
        let automation_fires = automation_fires(&discovered, &routine)?;
        let last_fire = newest_fire(
            cron_fires.first().cloned(),
            automation_fires.first().cloned(),
        );
        let (recent_fires, failure_streak) = fire_history(cron_fires, automation_fires);
        statuses.push(RoutineStatus {
            routine,
            paused_at,
            first_observed_at: cursor.as_ref().map(|cursor| cursor.baseline_at.clone()),
            last_evaluated_slot: cursor.and_then(|cursor| cursor.last_slot),
            next_due,
            last_fire,
            recent_fires,
            failure_streak,
            automation,
        });
    }

    Ok(RoutineStatusReport {
        machine_name: local_machine.machine_name,
        machine_id: local_machine.machine_id,
        statuses,
        retired: collection.retired,
        load_errors,
        owner_only: host.owner_only,
    })
}

/// Recent runs a state- or delivery-triggered routine's automation admitted.
///
/// Those fires never touch the cron fire store: the run itself carries the
/// routine as its trigger, so the run record is the fire record. A cron-only
/// routine has no such runs and is not queried.
fn automation_fires(
    discovered: &DiscoveredWorkspaces,
    routine: &LoadedRoutine,
) -> Result<Vec<RoutineFireRecord>, OrbitError> {
    let trigger = &routine.definition.trigger;
    if trigger.deliveries_landed.is_none() && trigger.state.is_none() {
        return Ok(Vec::new());
    }
    let Some((_, runtime)) = discovered
        .entries
        .iter()
        .find(|(_, runtime)| runtime.shared_root() == routine.source_orbit_dir)
    else {
        return Ok(Vec::new());
    };
    let runs = runtime
        .stores()
        .jobs()
        .list_job_runs_filtered(&JobRunQuery {
            job_id: Some(routine.definition.target.job_name().to_string()),
            trigger_routine: Some(routine.definition.name.clone()),
            limit: Some(FIRE_HISTORY_LIMIT),
            include_steps: false,
            ..JobRunQuery::default()
        })?;
    Ok(runs
        .into_iter()
        .map(|run| fire_from_run(&routine.definition.name, &routine.source_workspace, run))
        .collect())
}

fn fire_at(fire: &RoutineFireRecord) -> Option<DateTime<chrono::FixedOffset>> {
    DateTime::parse_from_rfc3339(&fire.slot).ok()
}

/// Merge histories without counting cron retries or a run present in both
/// stores twice. Never cross a saturated source's horizon: an unseen success
/// there could break the streak before an older fire from the other source.
fn fire_history(
    cron: Vec<RoutineFireRecord>,
    automation: Vec<RoutineFireRecord>,
) -> (Vec<RoutineFireRecord>, RoutineFailureStreak) {
    let horizon = [&cron, &automation]
        .into_iter()
        .filter(|fires| fires.len() == FIRE_HISTORY_LIMIT)
        .filter_map(|fires| fires.iter().filter_map(fire_at).min())
        .max();
    let cron_runs: HashSet<_> = cron.iter().filter_map(|fire| fire.run_id.clone()).collect();
    let mut slots = HashSet::new();
    let mut runs = HashSet::new();
    let mut fires: Vec<_> = cron
        .into_iter()
        // The store orders attempts descending within each slot.
        .filter(|fire| slots.insert(fire_at(fire)))
        .chain(automation.into_iter().filter(|fire| {
            fire.run_id
                .as_ref()
                .is_none_or(|id| !cron_runs.contains(id))
        }))
        .filter(|fire| {
            fire.run_id
                .as_ref()
                .is_none_or(|id| runs.insert(id.clone()))
        })
        .filter(|fire| horizon.is_none_or(|at| fire_at(fire).is_some_and(|slot| slot >= at)))
        .collect();
    fires.sort_by_key(|fire| std::cmp::Reverse(fire_at(fire)));
    let count = fires
        .iter()
        .take_while(|fire| {
            matches!(
                fire.state,
                RoutineFireState::Failed | RoutineFireState::TimedOut | RoutineFireState::Error
            )
        })
        .count();
    let streak = RoutineFailureStreak {
        count,
        since: count.checked_sub(1).map(|index| fires[index].slot.clone()),
        truncated: count > 0 && count == fires.len() && horizon.is_some(),
    };
    fires.truncate(RECENT_FIRE_LIMIT);
    (fires, streak)
}

fn fire_from_run(name: &str, source_workspace: &str, run: JobRun) -> RoutineFireRecord {
    let state = match run.state {
        JobRunState::Pending | JobRunState::Running | JobRunState::Retrying => {
            RoutineFireState::Dispatched
        }
        JobRunState::Success | JobRunState::Held => RoutineFireState::Succeeded,
        JobRunState::Skipped => RoutineFireState::Skipped,
        JobRunState::Timeout => RoutineFireState::TimedOut,
        JobRunState::Failed | JobRunState::Cancelled | JobRunState::Interrupted => {
            RoutineFireState::Failed
        }
    };
    let updated = run.finished_at.or(run.started_at).unwrap_or(run.created_at);
    RoutineFireRecord {
        routine_name: name.to_string(),
        slot: run.created_at.to_rfc3339(),
        attempt: run.attempt,
        state,
        run_id: Some(run.run_id),
        source_workspace: source_workspace.to_string(),
        detail: None,
        created_at: run.created_at.to_rfc3339(),
        updated_at: updated.to_rfc3339(),
    }
}

/// The later of a cron fire and an automation-admitted run, by creation time.
fn newest_fire(
    cron: Option<RoutineFireRecord>,
    automation: Option<RoutineFireRecord>,
) -> Option<RoutineFireRecord> {
    let (Some(cron_fire), Some(auto_fire)) = (&cron, &automation) else {
        return cron.or(automation);
    };
    let at = |fire: &RoutineFireRecord| DateTime::parse_from_rfc3339(&fire.created_at).ok();
    match (at(cron_fire), at(auto_fire)) {
        (Some(cron_at), Some(auto_at)) if auto_at <= cron_at => cron,
        (Some(_), Some(_)) => automation,
        _ => cron,
    }
}

/// A selected checkout is the entire discovery scope: never another store by
/// cwd, never a client-supplied path.
struct SelectedCheckout<'a>(&'a OrbitRuntime);

impl RoutineWorkspaceProvider for SelectedCheckout<'_> {
    fn discover_workspaces(&self, _: &Path) -> Result<DiscoveredWorkspaces, OrbitError> {
        let runtime = self.0;
        let workspace = Workspace {
            id: runtime.workspace_id()?,
            name: runtime.workspace_label(),
            owner_machine_id: runtime
                .workspace_runtime_binding()
                .and_then(|binding| binding.owner_machine_id.clone()),
            git_remote: None,
            ship_mode: None,
            base_branch: runtime.workspace_base_branch().into(),
            status: WorkspaceStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        Ok(DiscoveredWorkspaces::single_checkout(
            workspace,
            runtime.clone(),
        ))
    }
}

/// Routine status for one selected checkout, with the same replica rule the
/// host sweep applies.
pub(crate) fn checkout_routine_statuses(
    runtime: &OrbitRuntime,
) -> Result<RoutineStatusReport, OrbitError> {
    routine_statuses_with_providers(
        &runtime.global_root(),
        RoutineMachineIdentity {
            machine_id: runtime
                .automation_machine_identity()
                .unwrap_or("local")
                .into(),
            machine_name: "Selected host".into(),
        },
        &SelectedCheckout(runtime),
        Utc::now(),
    )
}

/// Toggle one routine definition in a selected checkout [ORB-14173].
///
/// An owner checkout keeps the general coordination-write guard. A replica
/// checkout may change only the replica-local definitions it schedules for
/// itself; a definition its owner schedules is refused with the owner named,
/// and a claimed worker never toggles anything.
pub(crate) fn toggle_checkout_routine(
    runtime: &OrbitRuntime,
    name: &str,
    target: &str,
    expected_enabled: bool,
    enabled: bool,
) -> Result<RoutineToggleOutcome, OrbitError> {
    let replica =
        runtime.worker_invocation().is_none() && runtime.coordination_write_owner().is_some();
    if !replica {
        runtime.ensure_coordination_task_write_permitted()?;
    }
    let report = checkout_routine_statuses(runtime)?;
    match toggle_listed_routine(&report, name, None, target, expected_enabled, enabled)? {
        Ok(outcome) => Ok(outcome),
        Err(RoutineToggleRefusal::OwnerAuthority { reason, .. }) => {
            Err(OrbitError::CapabilityRefused(reason))
        }
        Err(RoutineToggleRefusal::NotFound | RoutineToggleRefusal::WorkspaceMismatch { .. }) => {
            Err(OrbitError::InvalidInput(
                "routine unavailable in this workspace".into(),
            ))
        }
    }
}

/// Why a toggle selection was refused before any definition was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutineToggleRefusal {
    /// A replica lists the routine, but only its owner may change it.
    OwnerAuthority {
        /// Operator-facing explanation naming the owner.
        reason: String,
        /// The machine that owns the workspace's coordination store.
        owner_machine: String,
    },
    /// The report lists no schedulable routine of that name.
    NotFound,
    /// The routine comes from another source workspace than the caller's.
    WorkspaceMismatch {
        /// The routine's actual source workspace name.
        source_workspace: String,
    },
}

/// Select routine `name` from `report` and toggle its versioned `enabled`.
///
/// The one selection every toggle surface runs: an owner-only definition is
/// refused with the owner named, an unlisted name is not found, `source`
/// (workspace name and `.orbit` directory, when the report spans more than
/// the caller's checkout) must match the routine's, and the caller's
/// `target` must still be the definition's before [`set_routine_enabled`]
/// writes. The outer error is a failed write.
pub fn toggle_listed_routine(
    report: &RoutineStatusReport,
    name: &str,
    source: Option<(&str, &Path)>,
    target: &str,
    expected_enabled: bool,
    enabled: bool,
) -> Result<Result<RoutineToggleOutcome, RoutineToggleRefusal>, OrbitError> {
    if let Some(owned) = report
        .owner_only
        .iter()
        .find(|owned| owned.routine.definition.name == name)
    {
        return Ok(Err(RoutineToggleRefusal::OwnerAuthority {
            reason: owned.reason.clone(),
            owner_machine: owned.owner_machine.clone(),
        }));
    }
    let Some(status) = report
        .statuses
        .iter()
        .find(|status| status.routine.definition.name == name)
    else {
        return Ok(Err(RoutineToggleRefusal::NotFound));
    };
    let routine = &status.routine;
    if source.is_some_and(|(workspace, orbit_dir)| {
        routine.source_workspace != workspace || routine.source_orbit_dir != orbit_dir
    }) {
        return Ok(Err(RoutineToggleRefusal::WorkspaceMismatch {
            source_workspace: routine.source_workspace.clone(),
        }));
    }
    if routine.definition.target.as_ref_string() != target {
        return Ok(Ok(RoutineToggleOutcome::TargetConflict {
            actual_target: routine.definition.target.clone(),
        }));
    }
    set_routine_enabled(routine, expected_enabled, enabled).map(Ok)
}

/// The routine's next scheduled occurrence, rendered host-local.
///
/// This is the schedule coming around again, not the sweep's catch-up
/// eligibility: a routine holding a missed slot under `catch_up_once` is due
/// for that earlier slot while this still points forward. The projection comes
/// from the shared cron owner so routine status and auto-task status pin slots
/// to the minute identically. An unparseable cron projects nothing; the display
/// state reports why.
pub(crate) fn next_scheduled_occurrence(cron: &str, now: &DateTime<Local>) -> Option<String> {
    parse_cron(cron)
        .and_then(|cron| next_occurrence(&cron, now))
        .ok()
        .map(|slot| slot.to_rfc3339())
}

/// Optimistic outcome for a versioned routine-definition toggle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutineToggleOutcome {
    /// The definition was atomically changed.
    Changed,
    /// The desired state already matched the current definition.
    Unchanged,
    /// The caller's expected state was stale, so no write occurred.
    Conflict { actual_enabled: bool },
    /// The definition now dispatches a different target than the one the
    /// caller selected, so no write occurred.
    TargetConflict { actual_target: RoutineTarget },
}

/// Change only the typed `enabled` field of a routine definition.
///
/// The path comes from a freshly loaded [`LoadedRoutine`], never from a
/// transport payload. That routine is also the caller's selection: a file
/// retargeted since it was loaded is refused before its `enabled` state is
/// considered. The surgical edit preserves comments and field ordering;
/// the rewritten document is parsed and compared before the atomic rename so a
/// toggle cannot accidentally alter any other routine behavior.
pub fn set_routine_enabled(
    routine: &LoadedRoutine,
    expected_enabled: bool,
    enabled: bool,
) -> Result<RoutineToggleOutcome, OrbitError> {
    let raw = std::fs::read_to_string(&routine.path)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", routine.path.display())))?;
    let current = parse_routine_yaml(&raw)?;
    if current.name != routine.definition.name {
        return Err(OrbitError::InvalidInput(format!(
            "routine definition at {} changed identity from '{}' to '{}'",
            routine.path.display(),
            routine.definition.name,
            current.name
        )));
    }
    if current.target != routine.definition.target {
        return Ok(RoutineToggleOutcome::TargetConflict {
            actual_target: current.target,
        });
    }
    if current.enabled != expected_enabled {
        return Ok(RoutineToggleOutcome::Conflict {
            actual_enabled: current.enabled,
        });
    }
    if current.enabled == enabled {
        return Ok(RoutineToggleOutcome::Unchanged);
    }

    let rendered = rewrite_enabled_line(&raw, enabled)?;
    let rewritten = parse_routine_yaml(&rendered)?;
    let mut expected = current;
    expected.enabled = enabled;
    if rewritten != expected {
        return Err(OrbitError::Execution(
            "routine toggle validation changed fields other than enabled".to_string(),
        ));
    }
    atomic_write_text(&routine.path, &rendered)
        .map_err(|error| OrbitError::Io(format!("write {}: {error}", routine.path.display())))?;
    Ok(RoutineToggleOutcome::Changed)
}

/// Rewrite only the top-level `enabled:` line of a routine document, keeping
/// every other byte — comments, ordering, trailing comment on that line —
/// intact. Managed-routine refresh uses it too, so a shipped-template change
/// never silently flips an operator's opt-in back to the template default.
pub(crate) fn rewrite_enabled_line(raw: &str, enabled: bool) -> Result<String, OrbitError> {
    let newline = if raw.contains("\r\n") { "\r\n" } else { "\n" };
    let has_enabled = raw
        .lines()
        .any(|line| line.trim_end_matches('\r').starts_with("enabled:"));
    let mut rendered = String::with_capacity(raw.len() + 16);
    let mut replaced = false;
    for line in raw.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        let ending = &line[content.len()..];
        if !replaced && content.starts_with("enabled:") {
            let suffix = content
                .split_once('#')
                .map(|(_, comment)| format!(" # {}", comment.trim()))
                .unwrap_or_default();
            rendered.push_str(&format!("enabled: {enabled}{suffix}{ending}"));
            replaced = true;
        } else {
            rendered.push_str(line);
            if !has_enabled && !replaced && content.starts_with("name:") {
                if ending.is_empty() {
                    rendered.push_str(newline);
                }
                rendered.push_str(&format!("enabled: {enabled}{newline}"));
                replaced = true;
            }
        }
    }
    if !replaced {
        return Err(OrbitError::InvalidInput(
            "routine definition has no canonical top-level `name:` or `enabled:` field".to_string(),
        ));
    }
    Ok(rendered)
}

/// Pause a routine on this host (host-local, never synced). Returns `false`
/// when it was already paused.
pub fn pause_routine(global_root: &Path, name: &str, actor: &str) -> Result<bool, OrbitError> {
    let store = super::open_routine_store(global_root)?;
    store.routine_pause(name, actor)
}

/// Clear a host-local pause. Returns `false` when it was not paused.
pub fn resume_routine(global_root: &Path, name: &str) -> Result<bool, OrbitError> {
    let store = super::open_routine_store(global_root)?;
    store.routine_resume(name)
}

/// Recent fire attempts for one routine, newest first (for `routine show`).
pub fn recent_fires(
    global_root: &Path,
    name: &str,
    limit: usize,
) -> Result<Vec<RoutineFireRecord>, OrbitError> {
    let store = super::open_routine_store(global_root)?;
    store.routine_recent_fires(name, limit)
}
