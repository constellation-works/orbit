use super::*;

use super::fires::{FIRE_HISTORY_LIMIT, automation_fires, fire_history, newest_fire};

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
            super::super::open_routine_store(global_root)?
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
        // A condition routine's state: ownership, arm counts, any stall and
        // the last fire and skip with its reason.
        let automation = automation.or_else(|| {
            routine.definition.trigger.condition.as_ref()?;
            Some(
                discovered
                    .entries
                    .iter()
                    .find(|(_, runtime)| runtime.shared_root() == routine.source_orbit_dir)
                    .map_or_else(
                        || serde_json::json!({"reason": "source_unavailable"}),
                        |(_, runtime)| match runtime.routine_condition_listing(&routine.definition)
                        {
                            Ok(listing) => serde_json::json!({
                                "reason": listing
                                    .as_ref()
                                    .and_then(|listing| listing.stall.as_ref())
                                    .map_or("condition", |stall| stall.reason.as_str()),
                                "condition": listing,
                            }),
                            Err(error) => serde_json::json!({
                                "reason": "state_unavailable",
                                "error": error.to_string(),
                            }),
                        },
                    ),
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
