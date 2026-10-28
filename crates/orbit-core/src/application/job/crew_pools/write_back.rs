//! `task.crew` names the crew of the task's latest delivery [ORB-19416].
//! Admission writes the crew it chose back to the task when it differs from
//! the recorded one: an experiment arm's crew overlay, a provider hold or
//! limit, a usage-pressure or pool draw, or an explicit run crew. The write
//! lands only after the run is durably admitted, and only while the task still
//! names the crew the draw read, so a refused or duplicate submission and a
//! newer change to the task leave it alone [ORB-19485]. A claimed leaf draws
//! its crew on its executor, so its owner writes the leaf's crew back when the
//! leaf settles. Each write sets a `crew_source` naming why and appends a
//! `crew_assigned` history entry, so history keeps every earlier crew. A
//! one-time backfill repairs tasks admitted before the write-back.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::TaskHistoryEntry;
use orbit_types::workflow::{JobRun, JobRunState};
use serde::Serialize;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::contracts::{
    BACKFILL_CREW_SOURCE, REDRAW_SOURCE_PREFIX, experiment_crew_source,
};
use crate::application::task::{SYSTEM_ACTOR_LABEL, TaskRecordUpdateParams, TaskRecords};
use crate::runtime::run_input::{non_empty, singular_task_id_from_input};

use super::types::{POLICY_PIPELINES, SELECTION_KEY};

/// The provenance a crew written back to its task records, and the clause
/// its history entry explains the change with.
pub(crate) struct AdmittedCrewSource {
    source: String,
    cause: String,
}

impl AdmittedCrewSource {
    /// An experiment arm's crew overlay chose the crew.
    pub(crate) fn experiment(experiment: &str, arm: &str) -> Self {
        Self {
            source: experiment_crew_source(experiment, arm),
            cause: format!("by experiment {experiment} arm {arm}"),
        }
    }

    /// Admission drew another crew than the recorded one; `selection` is the
    /// draw's provenance as its `crew_selection` records it.
    pub(crate) fn redraw(selection: &str, explicit_run_crew: bool) -> Self {
        let reason = if explicit_run_crew {
            "run_crew"
        } else if selection.contains("provider hold excludes") {
            "provider_hold"
        } else if selection.contains("; provider limit:") {
            "provider_limit"
        } else if selection.contains("usage pressure") {
            "usage_pressure"
        } else {
            "pool"
        };
        Self::redraw_reason(reason)
    }

    fn redraw_reason(reason: &str) -> Self {
        Self {
            source: format!("{REDRAW_SOURCE_PREFIX}{reason}"),
            cause: format!("at admission ({reason})"),
        }
    }
}

/// The crew an admission drew, with the crew its task named when the draw read
/// it, held until the run is admitted [ORB-19485].
pub(crate) struct PendingCrewWriteBack {
    pub(super) recorded: Option<String>,
    pub(super) crew: String,
    pub(super) written: AdmittedCrewSource,
}

/// What a locked crew write requires of the task before it changes the task.
#[derive(Clone, Copy)]
enum CrewWriteGuard<'a> {
    /// The backfill, which also repairs terminal tasks because analytics read them.
    Any,
    /// An admission's write-back, which leaves terminal tasks alone and any task
    /// whose crew is no longer `recorded`: a newer change to the task owns it.
    Recorded(Option<&'a str>),
}

/// What one task-crew backfill changed in this workspace.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TaskCrewBackfill {
    pub workspace: String,
    /// Tasks with a delivery run that recorded a crew selection.
    pub examined: usize,
    /// Tasks whose crew was set from their latest delivery run.
    pub changed: usize,
    /// Each changed task, oldest id first.
    pub tasks: Vec<TaskCrewChange>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskCrewChange {
    pub task_id: String,
    pub from: Option<String>,
    pub to: String,
}

impl OrbitRuntime {
    /// The write-back of a crew an admitted run drew, applied only while the
    /// task still names the crew the draw read [ORB-19485].
    pub(crate) fn write_back_admitted_task_crew(
        &self,
        task_id: &str,
        pending: &PendingCrewWriteBack,
    ) -> Result<bool, OrbitError> {
        self.write_task_crew(
            task_id,
            &pending.crew,
            &pending.written,
            CrewWriteGuard::Recorded(pending.recorded.as_deref()),
        )
        .map(|change| change.is_some())
    }

    /// The owner's write-back of the crew a claimed leaf reported at
    /// settlement. Best effort: the settlement already stands, and a later
    /// admission or the backfill writes the crew again.
    ///
    /// Fenced against replay [ORB-19486, ORB-19521]: `committed` is true
    /// only for the call whose settlement mutation the claim journal
    /// committed, so a replayed receipt never re-applies its attribution,
    /// even when the replay read the claim before another call settled it.
    /// `recorded_crew` is the task crew read before the settlement mutation;
    /// the write lands only while the task still names it, so a delayed
    /// write cannot overwrite a crew a newer claim, delivery or operator
    /// update recorded after that snapshot.
    pub(crate) fn write_back_claimed_leaf_crew(
        &self,
        task_id: &str,
        crew: Option<&str>,
        committed: bool,
        recorded_crew: Option<&str>,
    ) {
        if !committed {
            return;
        }
        let Some(crew) = crew.and_then(non_empty) else {
            return;
        };
        let written = match self.experiment_crew_pool(task_id) {
            Ok(Some(pool)) => AdmittedCrewSource::experiment(&pool.experiment, &pool.arm),
            Ok(None) => AdmittedCrewSource::redraw_reason("claimed_leaf"),
            Err(error) => {
                tracing::warn!(
                    task_id,
                    "could not read the task's experiment crew pool: {error}"
                );
                AdmittedCrewSource::redraw_reason("claimed_leaf")
            }
        };
        match self.write_task_crew(
            task_id,
            crew,
            &written,
            CrewWriteGuard::Recorded(recorded_crew),
        ) {
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    task_id,
                    "could not write the claimed leaf's crew back to its task: {error}"
                );
            }
        }
    }

    /// Set each task's crew from its latest delivery's crew selection where
    /// they differ, with `crew_source` `backfill:admission` and a history
    /// entry [ORB-19416]. The latest delivery is the task's newest local
    /// delivery run or, for work a claimed leaf executed, its settled claim:
    /// the owner keeps no local run for those, only the settlement record
    /// [ORB-19602]. Terminal tasks are included, since analytics read their
    /// crew. A task is left alone when its latest local run was cancelled
    /// before implementing, so a crew that never ran cannot replace the
    /// task's, and when its explicit crew assignment is newer than the
    /// delivery it would copy from. Running it again changes nothing.
    pub fn backfill_admitted_task_crews(&self) -> Result<TaskCrewBackfill, OrbitError> {
        let mut latest_run: BTreeMap<String, JobRun> = BTreeMap::new();
        for job in POLICY_PIPELINES {
            let runs = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
                job_id: Some(job.to_string()),
                include_steps: false,
                ..JobRunQuery::default()
            })?;
            for run in runs {
                let Some(task_id) = run_task_id(&run) else {
                    continue;
                };
                match latest_run.get(&task_id) {
                    Some(seen) if seen.created_at >= run.created_at => {}
                    _ => {
                        latest_run.insert(task_id, run);
                    }
                }
            }
        }
        let mut latest_settlement: BTreeMap<String, (String, chrono::DateTime<chrono::Utc>)> =
            BTreeMap::new();
        for inspection in self.inspect_execution_claims()? {
            let task_id = inspection.claim.task_id.clone();
            let Some(settlement) = inspection.settlement.as_ref() else {
                continue;
            };
            let Some(crew) = settlement
                .crew
                .clone()
                .filter(|crew| !crew.trim().is_empty())
            else {
                continue;
            };
            let Ok(at) = chrono::DateTime::parse_from_rfc3339(&settlement.settled_at) else {
                continue;
            };
            let at = at.with_timezone(&chrono::Utc);
            match latest_settlement.get(&task_id) {
                Some((_, seen)) if *seen >= at => {}
                _ => {
                    latest_settlement.insert(task_id, (crew, at));
                }
            }
        }
        let mut report = TaskCrewBackfill {
            workspace: self.workspace_id()?,
            ..TaskCrewBackfill::default()
        };
        let written = AdmittedCrewSource {
            source: BACKFILL_CREW_SOURCE.to_string(),
            cause: "by backfill from its latest delivery's crew selection".to_string(),
        };
        let tasks: std::collections::BTreeSet<String> = latest_run
            .keys()
            .chain(latest_settlement.keys())
            .cloned()
            .collect();
        for task_id in tasks {
            // Whether the latest local run was cancelled before implementing:
            // its crew never ran [ORB-19602]. Step hydration is read only
            // for a cancelled latest run; every other run needs no steps.
            let cancelled = match latest_run.get(&task_id) {
                Some(run) => self.run_cancelled_before_implementation(run)?,
                None => false,
            };
            // The local run this would copy from, when its selection names a
            // crew and it was not cancelled before implementing.
            let run_source: Option<(String, chrono::DateTime<chrono::Utc>)> =
                match latest_run.get(&task_id) {
                    Some(run) if !cancelled => self
                        .run_selected_crew(&task_id, run)?
                        .map(|crew| (crew, run.created_at)),
                    _ => None,
                };
            let settlement_source = latest_settlement.get(&task_id);
            // A latest run cancelled before implementing ends the task here
            // unless a claimed leaf settled after it: an older run's crew is
            // not the latest delivery [ORB-19602].
            let settlement_supersedes = match (latest_run.get(&task_id), settlement_source) {
                (Some(run), Some((_, settled_at))) => *settled_at > run.created_at,
                (None, Some(_)) => true,
                _ => false,
            };
            if cancelled && !settlement_supersedes {
                report.examined += 1;
                continue;
            }
            // The newer delivery wins; a run and a settlement stamped alike
            // keep the run, the long-standing source.
            let (crew, at) = match (&run_source, settlement_source) {
                (Some((run_crew, run_at)), Some((settlement_crew, settled_at)))
                    if *settled_at > *run_at =>
                {
                    (settlement_crew.clone(), *settled_at)
                }
                (Some((run_crew, run_at)), _) => (run_crew.clone(), *run_at),
                (None, Some((settlement_crew, settled_at))) => {
                    (settlement_crew.clone(), *settled_at)
                }
                (None, None) => continue,
            };
            report.examined += 1;
            if self.backfill_explicit_is_newer(&task_id, at)? {
                continue;
            }
            match self.write_task_crew(&task_id, &crew, &written, CrewWriteGuard::Any) {
                Ok(Some(from)) => {
                    report.changed += 1;
                    report.tasks.push(TaskCrewChange {
                        task_id,
                        from,
                        to: crew,
                    });
                }
                Ok(None) => {}
                Err(OrbitError::NotFound { .. }) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(report)
    }

    /// Whether the task's explicit crew assignment is newer than the delivery
    /// `at` the backfill would copy from, so the operator's choice stands
    /// [ORB-19602].
    fn backfill_explicit_is_newer(
        &self,
        task_id: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, OrbitError> {
        let task = match self.get_task(task_id) {
            Ok(task) => task,
            Err(OrbitError::NotFound { .. }) => return Ok(false),
            Err(error) => return Err(error),
        };
        if task.crew_source.as_deref() != Some("explicit") {
            return Ok(false);
        }
        let history = self.get_task_history(task_id)?;
        let latest = history
            .iter()
            .filter(|entry| entry.event == "crew_assigned")
            .max_by_key(|entry| entry.at);
        Ok(latest.is_some_and(|entry| entry.at > at))
    }

    /// The crew `run` selected for `task_id`: its admission selection, else
    /// the selection a claimed leaf froze in its pipeline state.
    fn run_selected_crew(&self, task_id: &str, run: &JobRun) -> Result<Option<String>, OrbitError> {
        let selected = |selection: Option<&Value>| {
            selection
                .filter(|selection| {
                    selection.get("task_id").and_then(Value::as_str) == Some(task_id)
                })
                .and_then(|selection| selection.get("crew"))
                .and_then(Value::as_str)
                .and_then(non_empty)
                .map(ToOwned::to_owned)
        };
        if let Some(crew) = selected(
            run.input
                .as_ref()
                .and_then(|input| input.get(SELECTION_KEY)),
        ) {
            return Ok(Some(crew));
        }
        Ok(self
            .read_run_state(&run.run_id)?
            .and_then(|state| selected(state.crew_selection.as_ref())))
    }

    /// The locked write behind every write-back. `Some(previous crew)` when
    /// it wrote.
    fn write_task_crew(
        &self,
        task_id: &str,
        crew: &str,
        written: &AdmittedCrewSource,
        guard: CrewWriteGuard<'_>,
    ) -> Result<Option<Option<String>>, OrbitError> {
        let Some(crew) = non_empty(crew) else {
            return Ok(None);
        };
        let mut changed = None;
        self.stores()
            .tasks()
            .with_task_write_lock(task_id, &mut || {
                let current = self.get_task(task_id)?;
                let refused = match guard {
                    CrewWriteGuard::Any => false,
                    CrewWriteGuard::Recorded(recorded) => {
                        current.status.is_terminal() || current.crew.as_deref() != recorded
                    }
                };
                if current.crew.as_deref() == Some(crew) || refused {
                    return Ok(());
                }
                let now = chrono::Utc::now();
                self.stores().task_records().update(
                    task_id,
                    TaskRecordUpdateParams {
                        actor: SYSTEM_ACTOR_LABEL.to_string(),
                        crew: Some(Some(crew.to_string())),
                        crew_source: Some(Some(written.source.clone())),
                        expected_status: Some(vec![current.status]),
                        append_history: vec![TaskHistoryEntry {
                            at: now,
                            by: SYSTEM_ACTOR_LABEL.to_string(),
                            event: "crew_assigned".to_string(),
                            // The new crew is the note's last backticked name,
                            // which attempt attribution reads.
                            note: Some(format!(
                                "crew changed from `{}` to `{crew}` {}",
                                current.crew.as_deref().unwrap_or("(none)"),
                                written.cause,
                            )),
                            from_status: None,
                            to_status: None,
                            operation_id: None,
                            decision: None,
                        }],
                        ..Default::default()
                    },
                )?;
                changed = Some(current.crew.clone());
                Ok(())
            })?;
        Ok(changed)
    }

    /// Whether the task's latest local `run` was cancelled before it
    /// implemented: no implement step completed, so its crew selection never
    /// ran [ORB-19602]. Step hydration is read only for a cancelled run;
    /// any other state needs no steps.
    fn run_cancelled_before_implementation(&self, run: &JobRun) -> Result<bool, OrbitError> {
        if run.state != JobRunState::Cancelled {
            return Ok(false);
        }
        let steps = match self.stores().jobs().get_job_run(&run.run_id)? {
            Some(full) => full.steps,
            None => Vec::new(),
        };
        Ok(!steps
            .iter()
            .any(|step| step.state == JobRunState::Success && step.target_id.contains("implement")))
    }
}

/// The one task a delivery run carries: its selection's task, else the
/// input's single task.
fn run_task_id(run: &JobRun) -> Option<String> {
    let input = run.input.as_ref()?;
    input
        .get(SELECTION_KEY)
        .and_then(|selection| selection.get("task_id"))
        .and_then(Value::as_str)
        .or_else(|| singular_task_id_from_input(input))
        .and_then(non_empty)
        .map(ToOwned::to_owned)
}
