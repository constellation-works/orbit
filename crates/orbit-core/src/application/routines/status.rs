//! Read-only status projection for `orbit routine list` / `show` [ORB-10021]:
//! every routine with both toggle layers (versioned `enabled` and host-local
//! pause), the computed next-due slot, and the last recorded fire — so "why
//! didn't this fire?" is one command.

use std::path::Path;

use chrono::{DateTime, Local, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_store::contracts::{JobRunQuery, RoutineFireRecord, RoutineFireState};
use orbit_types::workflow::{JobRun, JobRunState, RoutineTarget};
use orbit_types::workspace::{Workspace, WorkspaceStatus};

use super::RoutineMachineIdentity;
use super::due::{next_occurrence, parse_cron};
use super::loader::{
    DiscoveredWorkspaces, LoadedRoutine, OwnerOnlyRoutine, RetiredRoutine, RoutineLoadError,
    RoutineWorkspaceProvider, collect_host_routines,
};
use crate::OrbitRuntime;

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
    /// Source or trigger state cannot be shown as a next evaluation.
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
}

/// Collect routine status from a caller-supplied workspace provider. Registry
/// ownership remains outside Core.
pub fn routine_statuses_with_providers(
    global_root: &Path,
    local_machine: RoutineMachineIdentity,
    workspace_provider: &dyn RoutineWorkspaceProvider,
    now_utc: DateTime<Utc>,
) -> Result<RoutineStatusReport, OrbitError> {
    let store = super::open_routine_store(global_root)?;

    let discovered = workspace_provider.discover_workspaces(global_root)?;
    let mut load_errors = discovered.errors.clone();
    let host = collect_host_routines(&discovered);
    let mut collection = host.collection;
    load_errors.append(&mut collection.errors);

    let pauses = store.routine_pauses()?;
    let now = now_utc.with_timezone(&Local);

    let mut statuses = Vec::with_capacity(collection.routines.len());
    for routine in collection.routines {
        let next_due = next_scheduled_occurrence(&routine.definition.trigger.cron, &now);
        let cron_fire = store.routine_latest_fire(&routine.definition.name)?;
        let cursor = store.routine_cursor(&routine.definition.name)?;
        let paused_at = pauses
            .get(&routine.definition.name)
            .map(|pause| pause.paused_at.clone());
        let automation=(routine.definition.trigger.deliveries_landed.is_some() || routine.definition.trigger.state.is_some()).then(|| {
            discovered.entries.iter().find(|(_,runtime)|runtime.shared_root()==routine.source_orbit_dir).map_or_else(||serde_json::json!({"reason":"source_unavailable"}),|(_,runtime)|match crate::application::automation::inspect_routine(runtime,&routine.definition,now_utc) {Ok(value)=>serde_json::json!(value),Err(error)=>serde_json::json!({"reason":"state_unavailable","error":error.to_string()})})
        });
        let last_fire = newest_fire(cron_fire, automation_fire(&discovered, &routine)?);
        statuses.push(RoutineStatus {
            routine,
            paused_at,
            first_observed_at: cursor.as_ref().map(|cursor| cursor.baseline_at.clone()),
            last_evaluated_slot: cursor.and_then(|cursor| cursor.last_slot),
            next_due,
            last_fire,
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

/// The latest run a state- or delivery-triggered routine's automation admitted.
///
/// Those fires never touch the cron fire store: the run itself carries the
/// routine as its trigger, so the run record is the fire record. A cron-only
/// routine has no such runs and is not queried.
fn automation_fire(
    discovered: &DiscoveredWorkspaces,
    routine: &LoadedRoutine,
) -> Result<Option<RoutineFireRecord>, OrbitError> {
    let trigger = &routine.definition.trigger;
    if trigger.deliveries_landed.is_none() && trigger.state.is_none() {
        return Ok(None);
    }
    let Some((_, runtime)) = discovered
        .entries
        .iter()
        .find(|(_, runtime)| runtime.shared_root() == routine.source_orbit_dir)
    else {
        return Ok(None);
    };
    let run = runtime
        .stores()
        .jobs()
        .list_job_runs_filtered(&JobRunQuery {
            job_id: Some(routine.definition.target.job_name().to_string()),
            trigger_routine: Some(routine.definition.name.clone()),
            limit: Some(1),
            include_steps: false,
            ..JobRunQuery::default()
        })?
        .into_iter()
        .next();
    Ok(run.map(|run| fire_from_run(&routine.definition.name, &routine.source_workspace, run)))
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
    if let Some(owned) = report
        .owner_only
        .iter()
        .find(|owned| owned.routine.definition.name == name)
    {
        return Err(OrbitError::CapabilityRefused(owned.reason.clone()));
    }
    let status = report
        .statuses
        .iter()
        .find(|status| status.routine.definition.name == name)
        .ok_or_else(|| OrbitError::InvalidInput("routine unavailable in this workspace".into()))?;
    if status.routine.definition.target.as_ref_string() != target {
        return Ok(RoutineToggleOutcome::TargetConflict {
            actual_target: status.routine.definition.target.clone(),
        });
    }
    set_routine_enabled(&status.routine, expected_enabled, enabled)
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
