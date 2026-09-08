//! Read-only status projection for `orbit routine list` / `show` [ORB-10021]:
//! every routine with all three toggle layers (versioned `enabled`, versioned
//! host pinning, host-local pause), the computed next-due slot, and the last
//! recorded fire — so "why didn't this fire?" is one command.

use std::path::Path;

use chrono::{DateTime, Local, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_common::protocol::yaml::{parse_local_routine_yaml, parse_routine_yaml};
use orbit_store::contracts::RoutineFireRecord;

use super::due::{next_occurrence, parse_cron};
use super::loader::{LoadedRoutine, RoutineLoadError, RoutineWorkspaceProvider, collect_routines};
use super::validation::{
    RoutinePinValidation, RoutinePlacementProjection, RoutinePlacementProvider,
    RoutineRegistryStatus, validate_routine_pins,
};

/// Operator-facing schedule readiness. Theoretical next-slot math may still be
/// present; this state says whether that time is armed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleDisplayState {
    /// Enabled, eligible, and a next slot is a real scheduled evaluation.
    Scheduled,
    /// Definition `enabled` is false. A next slot, if present, is hypothetical.
    Disabled,
    /// Host-local pause. A next slot, if present, is hypothetical.
    Paused,
    /// Enabled delivery- or state-triggered work is waiting on that trigger.
    Waiting,
    /// The scheduler has never recorded a cursor for this definition.
    NeverObserved,
    /// Pin, source, or trigger state cannot be shown as a next evaluation.
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
    /// Whether this host's `host_id` appears in the routine's `hosts`.
    pub pinned_to_host: bool,
    /// Registry-aware pin eligibility and additive diagnostics.
    pub validation: RoutinePinValidation,
    /// Host-local pause, when one is set (RFC 3339 pause timestamp).
    pub paused_at: Option<String>,
    /// First scheduler observation on this host (RFC 3339).
    pub first_observed_at: Option<String>,
    /// Most recent scheduled slot consumed by the scheduler (RFC 3339).
    pub last_evaluated_slot: Option<String>,
    /// Next scheduled slot (RFC 3339, host-local), when computable.
    pub next_due: Option<String>,
    /// Most recent fire attempt recorded on this host.
    pub last_fire: Option<RoutineFireRecord>,
    pub automation: Option<serde_json::Value>,
}

impl RoutineStatus {
    /// Whether the routine would currently fire on this host when due:
    /// enabled, pinned, and not paused.
    pub fn effective(&self) -> bool {
        self.routine.definition.enabled && self.pinned_to_host && self.paused_at.is_none()
    }

    /// How Operations (and other projections) should label the next slot.
    pub fn schedule_display_state(&self) -> ScheduleDisplayState {
        if !self.routine.definition.enabled {
            return ScheduleDisplayState::Disabled;
        }
        if self.paused_at.is_some() {
            return ScheduleDisplayState::Paused;
        }
        if !self.pinned_to_host {
            return ScheduleDisplayState::Unavailable;
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
    /// This host's identity.
    pub host_id: String,
    /// Stable machine identity used by registry-resolved pins.
    pub machine_id: String,
    /// Registry source/state used by this projection.
    pub registry: RoutineRegistryStatus,
    /// Per-routine status rows, in discovery order.
    pub statuses: Vec<RoutineStatus>,
    /// Fail-closed load failures (these routines are absent).
    pub load_errors: Vec<RoutineLoadError>,
}

/// Collect routine status from caller-supplied placement and workspace
/// providers. Registry/cache ownership remains outside Core.
pub fn routine_statuses_with_providers(
    global_root: &Path,
    placement_provider: &dyn RoutinePlacementProvider,
    workspace_provider: &dyn RoutineWorkspaceProvider,
    now_utc: DateTime<Utc>,
) -> Result<RoutineStatusReport, OrbitError> {
    let store = super::open_routine_store(global_root)?;
    let RoutinePlacementProjection {
        local_host,
        registry: registry_view,
    } = placement_provider.load_routine_placement()?;
    let registry = registry_view.status();

    let discovered = workspace_provider.discover_workspaces(global_root)?;
    let mut load_errors = discovered.errors.clone();
    let mut collection = collect_routines(&discovered.entries, &local_host.host_id);
    load_errors.append(&mut collection.errors);

    let pauses = store.routine_pauses()?;
    let now = now_utc.with_timezone(&Local);

    let mut statuses = Vec::with_capacity(collection.routines.len());
    for routine in collection.routines {
        let next_due = next_scheduled_occurrence(&routine.definition.trigger.cron, &now);
        let last_fire = store.routine_latest_fire(&routine.definition.name)?;
        let cursor = store.routine_cursor(&routine.definition.name)?;
        let paused_at = pauses
            .get(&routine.definition.name)
            .map(|pause| pause.paused_at.clone());
        let validation = validate_routine_pins(
            &local_host,
            routine.origin,
            &routine.definition.hosts,
            &registry_view,
        );
        let pinned_to_host = validation.eligible;
        let automation=(routine.definition.trigger.deliveries_landed.is_some() || routine.definition.trigger.state.is_some()).then(|| {
            discovered.entries.iter().find(|(_,runtime)|runtime.shared_root()==routine.source_orbit_dir).map_or_else(||serde_json::json!({"reason":"source_unavailable"}),|(_,runtime)|match crate::application::automation::inspect_routine(runtime,&routine.definition,now_utc) {Ok(value)=>serde_json::json!(value),Err(error)=>serde_json::json!({"reason":"state_unavailable","error":error.to_string()})})
        });
        statuses.push(RoutineStatus {
            routine,
            pinned_to_host,
            validation,
            paused_at,
            first_observed_at: cursor.as_ref().map(|cursor| cursor.baseline_at.clone()),
            last_evaluated_slot: cursor.and_then(|cursor| cursor.last_slot),
            next_due,
            last_fire,
            automation,
        });
    }

    Ok(RoutineStatusReport {
        host_id: local_host.host_id,
        machine_id: local_host.machine_id,
        registry,
        statuses,
        load_errors,
    })
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutineToggleOutcome {
    /// The definition was atomically changed.
    Changed,
    /// The desired state already matched the current definition.
    Unchanged,
    /// The caller's expected state was stale, so no write occurred.
    Conflict { actual_enabled: bool },
}

/// Change only the typed `enabled` field of a routine definition.
///
/// The path comes from a freshly loaded [`LoadedRoutine`], never from a
/// transport payload. The surgical edit preserves comments and field ordering;
/// the rewritten document is parsed and compared before the atomic rename so a
/// toggle cannot accidentally alter any other routine behavior.
pub fn set_routine_enabled(
    routine: &LoadedRoutine,
    local_host_id: &str,
    expected_enabled: bool,
    enabled: bool,
) -> Result<RoutineToggleOutcome, OrbitError> {
    let raw = std::fs::read_to_string(&routine.path)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", routine.path.display())))?;
    let current = parse_for_origin(&raw, routine.origin, local_host_id)?;
    if current.name != routine.definition.name {
        return Err(OrbitError::InvalidInput(format!(
            "routine definition at {} changed identity from '{}' to '{}'",
            routine.path.display(),
            routine.definition.name,
            current.name
        )));
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
    let rewritten = parse_for_origin(&rendered, routine.origin, local_host_id)?;
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

fn parse_for_origin(
    raw: &str,
    origin: super::loader::RoutineOrigin,
    local_host_id: &str,
) -> Result<orbit_types::workflow::RoutineDefinition, OrbitError> {
    match origin {
        super::loader::RoutineOrigin::Committed => parse_routine_yaml(raw),
        super::loader::RoutineOrigin::Local => parse_local_routine_yaml(raw, local_host_id),
    }
}

fn rewrite_enabled_line(raw: &str, enabled: bool) -> Result<String, OrbitError> {
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
