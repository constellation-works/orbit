use super::*;

pub(super) const FIRE_HISTORY_LIMIT: usize = 100;

pub(super) const RECENT_FIRE_LIMIT: usize = 10;

/// Recent runs a state-, delivery- or condition-triggered routine's
/// automation admitted.
///
/// Those fires never touch the cron fire store: the run itself carries the
/// routine as its trigger, so the run record is the fire record. A cron-only
/// routine has no such runs and is not queried.
pub(super) fn automation_fires(
    discovered: &DiscoveredWorkspaces,
    routine: &LoadedRoutine,
) -> Result<Vec<RoutineFireRecord>, OrbitError> {
    let trigger = &routine.definition.trigger;
    if trigger.deliveries_landed.is_none() && trigger.state.is_none() && trigger.condition.is_none()
    {
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

pub(super) fn fire_at(fire: &RoutineFireRecord) -> Option<DateTime<chrono::FixedOffset>> {
    DateTime::parse_from_rfc3339(&fire.slot).ok()
}

/// Merge histories without counting cron retries or a run present in both
/// stores twice. Never cross a saturated source's horizon: an unseen success
/// there could break the streak before an older fire from the other source.
pub(super) fn fire_history(
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

pub(super) fn fire_from_run(name: &str, source_workspace: &str, run: JobRun) -> RoutineFireRecord {
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
pub(super) fn newest_fire(
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

/// Recent fire attempts for one routine, newest first (for `routine show`).
pub fn recent_fires(
    global_root: &Path,
    name: &str,
    limit: usize,
) -> Result<Vec<RoutineFireRecord>, OrbitError> {
    let store = super::super::open_routine_store(global_root)?;
    store.routine_recent_fires(name, limit)
}
