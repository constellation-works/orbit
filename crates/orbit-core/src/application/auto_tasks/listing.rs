//! Whether each listed auto-task definition will fire on this host, decided
//! once. `orbit auto-task list`, MCP `orbit.auto_task.list` (plain and
//! bounded) and the dashboard's `GET /api/auto-tasks` render this decision and
//! derive none of it themselves, so they cannot disagree.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::AutoTaskSchedule;

use super::crud::require_loaded;
use super::loader::{AutoTaskCollection, AutoTaskLoadError};
use super::schedule::next_scheduled_slot;
use super::scheduler::ListedAutoTask;
use super::state::{AutoTaskCursor, cursor_state_path, load_cursor_state};
use crate::OrbitRuntime;
use crate::application::automation::delivery_ownership_refusal;
use crate::application::plugin::is_listed;
use crate::application::routines::ScheduleDisplayState;

/// Why a listed definition never fires on this host. The file stays exactly
/// where it is either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoTaskSkip {
    /// Its seeding plugin is off in this workspace or on the host.
    PluginInactive,
    /// An enabled delivery definition whose resolved owner is another machine,
    /// or none, so no tick here can admit work for it [ORB-12867].
    NotOwnedHere,
}

/// One listed definition and the decision every list surface reports for it.
#[derive(Debug, Clone)]
pub struct AutoTaskListingRow {
    pub listed: ListedAutoTask,
    /// The enabled state the scheduler acts on, which every listing filters
    /// and labels by ([`OrbitRuntime::auto_task_enabled`]).
    pub effective_enabled: bool,
    /// Why it never fires here, if it does not.
    pub skip: Option<AutoTaskSkip>,
    /// The operator-facing reason, set exactly when `skip` is.
    pub skipped_reason: Option<String>,
    /// This host's scheduler cursor, when the definition has been observed and
    /// the cursor state was readable.
    pub cursor: Option<AutoTaskCursor>,
    /// How the next evaluation is labelled.
    pub schedule_state: ScheduleDisplayState,
    /// The schedule's next occurrence: armed when `schedule_state` is
    /// `Scheduled`, hypothetical when it is `Disabled`, otherwise `None`.
    pub next_slot: Option<DateTime<Utc>>,
}

impl AutoTaskListingRow {
    /// The state and slot to show when the caller also inspected the delivery
    /// trigger (only the dashboard does, per row): a waiting definition whose
    /// trigger state could not be read is unavailable.
    pub fn schedule_with_trigger(
        &self,
        trigger_readable: bool,
    ) -> (ScheduleDisplayState, Option<DateTime<Utc>>) {
        if !trigger_readable && self.schedule_state == ScheduleDisplayState::Waiting {
            return (ScheduleDisplayState::Unavailable, None);
        }
        (self.schedule_state, self.next_slot)
    }
}

/// Every definition one listing shows, in load order.
#[derive(Debug)]
pub struct AutoTaskListing {
    pub rows: Vec<AutoTaskListingRow>,
    /// Definitions whose seeding plugin is off here, listed or not, so a
    /// surface can offer to show them.
    pub inactive_plugin_count: usize,
    /// Why the host cursor state could not be read. Every cron or interval
    /// enabled row is then `Unavailable` rather than silently never observed.
    /// Delivery schedules do not use time cursors and remain independent.
    pub cursor_state_error: Option<String>,
    /// Definition files that failed to load fail-closed.
    pub load_errors: Vec<AutoTaskLoadError>,
}

impl OrbitRuntime {
    /// The listing every auto-task list surface renders. A definition whose
    /// seeding plugin is off is omitted unless `include_inactive_plugins`
    /// asks for it. A discovery pass in which nothing loaded but a file failed
    /// is an error, as for [`OrbitRuntime::auto_task_list`].
    pub fn auto_task_listing_state(
        &self,
        include_inactive_plugins: bool,
        now: DateTime<Utc>,
    ) -> Result<AutoTaskListing, OrbitError> {
        let collection = self.collect_readable_auto_tasks();
        require_loaded(&collection)?;
        Ok(self.listing_from(collection, include_inactive_plugins, now))
    }

    /// The same listing for a surface that shows load errors itself (the
    /// dashboard panel): nothing that failed to load fails the listing.
    pub fn auto_task_listing_state_lenient(
        &self,
        include_inactive_plugins: bool,
        now: DateTime<Utc>,
    ) -> AutoTaskListing {
        let collection = self.collect_readable_auto_tasks();
        self.listing_from(collection, include_inactive_plugins, now)
    }

    fn listing_from(
        &self,
        collection: AutoTaskCollection,
        include_inactive_plugins: bool,
        now: DateTime<Utc>,
    ) -> AutoTaskListing {
        let cursors = load_cursor_state(&cursor_state_path(&self.paths().state_dir));
        let cursor_state_error = cursors.as_ref().err().map(ToString::to_string);
        let listed = collection
            .definitions
            .into_iter()
            .map(|loaded| self.listed_auto_task(loaded.definition))
            .collect::<Vec<_>>();
        let inactive_plugin_count = listed
            .iter()
            .filter(|listed| listed.inactive_plugin.is_some())
            .count();
        let rows = listed
            .into_iter()
            .filter(|listed| is_listed(listed.inactive_plugin.is_some(), include_inactive_plugins))
            .map(|listed| {
                let cursor = cursors
                    .as_ref()
                    .ok()
                    .and_then(|state| state.definitions.get(&listed.definition.name))
                    .cloned();
                self.listing_row(listed, cursor, cursor_state_error.is_some(), now)
            })
            .collect();
        AutoTaskListing {
            rows,
            inactive_plugin_count,
            cursor_state_error,
            load_errors: collection.errors,
        }
    }

    fn listing_row(
        &self,
        listed: ListedAutoTask,
        cursor: Option<AutoTaskCursor>,
        cursor_state_unavailable: bool,
        now: DateTime<Utc>,
    ) -> AutoTaskListingRow {
        let effective_enabled = self.auto_task_enabled(&listed.definition);
        let (skip, skipped_reason) = match &listed.skipped_reason {
            Some(reason) => (Some(AutoTaskSkip::PluginInactive), Some(reason.clone())),
            None => match delivery_ownership_refusal(self, &listed.definition) {
                Some(refusal) => (Some(AutoTaskSkip::NotOwnedHere), Some(refusal.reason())),
                None => (None, None),
            },
        };
        let (schedule_state, next_slot) = schedule_state(
            &listed.definition.schedule,
            effective_enabled,
            skip == Some(AutoTaskSkip::NotOwnedHere),
            cursor.as_ref(),
            cursor_state_unavailable,
            now,
        );
        AutoTaskListingRow {
            listed,
            effective_enabled,
            skip,
            skipped_reason,
            cursor,
            schedule_state,
            next_slot,
        }
    }
}

/// How the next evaluation is labelled, and the slot it names.
///
/// The slot is the schedule's next arrival, never catch-up eligibility: a
/// definition with a missed slot pending is due for that earlier slot while
/// this points forward. A schedule that cannot be projected (unparseable
/// cron, interval with no anchor) is `Unavailable` on its own row only. A
/// plugin-inactive definition keeps the state its schedule implies; every
/// surface reports that skip on its own.
fn schedule_state(
    schedule: &AutoTaskSchedule,
    effective_enabled: bool,
    not_owned_here: bool,
    cursor: Option<&AutoTaskCursor>,
    cursor_state_unavailable: bool,
    now: DateTime<Utc>,
) -> (ScheduleDisplayState, Option<DateTime<Utc>>) {
    let baseline = cursor.and_then(|cursor| {
        DateTime::parse_from_rfc3339(&cursor.baseline_at)
            .ok()
            .map(|baseline| baseline.with_timezone(&Utc))
    });
    let next = next_scheduled_slot(schedule, baseline, now).ok().flatten();
    if !effective_enabled {
        return (ScheduleDisplayState::Disabled, next);
    }
    if not_owned_here {
        return (ScheduleDisplayState::Unavailable, None);
    }
    match schedule {
        AutoTaskSchedule::Deliveries { .. } => (ScheduleDisplayState::Waiting, None),
        AutoTaskSchedule::Cron { .. } | AutoTaskSchedule::Interval { .. }
            if cursor_state_unavailable =>
        {
            (ScheduleDisplayState::Unavailable, None)
        }
        AutoTaskSchedule::Cron { .. } | AutoTaskSchedule::Interval { .. } => match (cursor, next) {
            (None, _) => (ScheduleDisplayState::NeverObserved, None),
            (Some(_), Some(at)) => (ScheduleDisplayState::Scheduled, Some(at)),
            (Some(_), None) => (ScheduleDisplayState::Unavailable, None),
        },
    }
}
