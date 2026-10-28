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
use orbit_types::workflow::JobRun;
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

    /// Set each task's crew from its latest delivery run's crew selection
    /// where they differ, with `crew_source` `backfill:admission` and a
    /// history entry [ORB-19416]. Terminal tasks are included, since
    /// analytics read their crew. Running it again changes nothing.
    pub fn backfill_admitted_task_crews(&self) -> Result<TaskCrewBackfill, OrbitError> {
        let mut latest: BTreeMap<String, JobRun> = BTreeMap::new();
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
                match latest.get(&task_id) {
                    Some(seen) if seen.created_at >= run.created_at => {}
                    _ => {
                        latest.insert(task_id, run);
                    }
                }
            }
        }
        let mut report = TaskCrewBackfill {
            workspace: self.workspace_id()?,
            ..TaskCrewBackfill::default()
        };
        let written = AdmittedCrewSource {
            source: BACKFILL_CREW_SOURCE.to_string(),
            cause: "by backfill from its latest delivery run's crew selection".to_string(),
        };
        for (task_id, run) in latest {
            let Some(crew) = self.run_selected_crew(&task_id, &run)? else {
                continue;
            };
            report.examined += 1;
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
