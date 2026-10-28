//! Sticky enrollment. Decisions execute under the store transaction,
//! so concurrent admissions and reweights cannot mix segments or surfaces.
use super::definition::{ExperimentOverlay, PopulationFilter};
use crate::OrbitRuntime;
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::{ExperimentAssignment, ExperimentRecord, ExperimentStoreBackend as _};
use orbit_types::task::Task;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// Why a task was excluded because another experiment owns one of its
/// surfaces.
pub(crate) const SURFACE_CONFLICT_EXCLUSION: &str = "surface_conflict";
/// Why a task with an explicit crew was excluded from an experiment whose
/// arms overlay the crew surface: every arm would run the explicit crew, so
/// a control task could run a treatment crew unseen [ORB-19416].
pub(crate) const EXPLICIT_CREW_EXCLUSION: &str = "explicit_crew";

impl OrbitRuntime {
    /// Assign eligible running experiments once, before any delivery crew draw.
    pub(crate) fn admit_task_experiments(
        &self,
        task: &Task,
    ) -> Result<Vec<ExperimentAssignment>, OrbitError> {
        let enabled = self.current_experiments_enabled();
        // Read before the store transaction: recovering a legacy crew's
        // provenance reads the task's history.
        let explicit_crew = task
            .crew
            .as_deref()
            .is_some_and(|crew| !crew.trim().is_empty())
            && self.task_crew_source(task)?.as_deref() == Some("explicit");
        self.sqlite_store()?.assign_experiments(
            &self.workspace_id()?,
            &task.id,
            true,
            enabled,
            &|records, existing, admitted| {
                // Old admission-only rows were created even when the feature
                // was disabled. Reconsider them until they have a sticky row.
                if (admitted && !existing.is_empty()) || task.status.is_terminal() {
                    return Ok(Vec::new());
                }
                let mut occupied = occupied_surfaces(records, existing)?;
                let mut additions = Vec::new();
                for record in records {
                    let experiment = &record.experiment;
                    if experiment.status != "running"
                        || !matches!(experiment.mode.as_str(), "shadow" | "live")
                        || existing.iter().any(|row| row.experiment == experiment.id)
                    {
                        continue;
                    }
                    let filter: PopulationFilter =
                        serde_json::from_str(&experiment.population_json).map_err(|error| {
                            OrbitError::Store(format!("invalid experiment population: {error}"))
                        })?;
                    if !population_matches(&filter, task) {
                        continue;
                    }
                    let surfaces = experiment_surfaces(record)?;
                    let exclusion_reason = if !occupied.is_disjoint(&surfaces) {
                        Some(SURFACE_CONFLICT_EXCLUSION.to_string())
                    } else if explicit_crew && surfaces.contains("crew") {
                        Some(EXPLICIT_CREW_EXCLUSION.to_string())
                    } else {
                        None
                    };
                    let mut assignment = ExperimentAssignment {
                        experiment: experiment.id.clone(),
                        task_id: task.id.clone(),
                        arm: None,
                        segment: None,
                        digest: None,
                        assigned_at: Utc::now(),
                        forced: false,
                        exclusion_reason,
                    };
                    if assignment.exclusion_reason.is_none() {
                        let arms = record
                            .arms
                            .iter()
                            .filter(|arm| arm.retired_segment.is_none())
                            .collect::<Vec<_>>();
                        let total: u64 = arms.iter().map(|arm| u64::from(arm.weight)).sum();
                        if total == 0 {
                            return Err(OrbitError::Store(
                                "running experiment has no weighted arms".into(),
                            ));
                        }
                        let digest = Sha256::digest(
                            format!("{}:{}:{}", experiment.salt, experiment.id, task.id).as_bytes(),
                        );
                        let mut bytes = [0u8; 8];
                        bytes.copy_from_slice(&digest[..8]);
                        let mut ticket = u64::from_be_bytes(bytes) % total;
                        let selected = arms
                            .into_iter()
                            .find(|arm| {
                                if ticket < u64::from(arm.weight) {
                                    true
                                } else {
                                    ticket -= u64::from(arm.weight);
                                    false
                                }
                            })
                            .ok_or_else(|| {
                                OrbitError::Store("experiment draw exhausted its arms".into())
                            })?;
                        assignment.arm = Some(selected.name.clone());
                        assignment.segment = Some(experiment.segment);
                        assignment.digest = Some(selected.overlay_digest.clone());
                        occupied.extend(surfaces);
                    }
                    additions.push(assignment);
                }
                Ok(additions)
            },
        )
    }

    /// Operator-only tool entrypoint: force a nonterminal task without an arm
    /// assignment into a running arm. An `explicit_crew` exclusion for this
    /// experiment yields to the force; an existing arm assignment or another
    /// exclusion for it remains sticky, and overlapping surfaces owned by
    /// another experiment are refused. A task with an explicit crew may be
    /// forced into a crew arm; the arm's crew then runs and is written back to
    /// the task at admission.
    pub fn force_experiment_assignment(
        &self,
        id: &str,
        task_id: &str,
        arm: &str,
    ) -> Result<Vec<ExperimentAssignment>, OrbitError> {
        let task = self.get_task(task_id)?;
        if task.status.is_terminal() {
            return Err(OrbitError::InvalidInput(
                "cannot assign an experiment to a terminal task".into(),
            ));
        }
        if !self.current_experiments_enabled() {
            return Err(OrbitError::PolicyDenied(
                "experiments.enabled is false".into(),
            ));
        }
        self.sqlite_store()?.assign_experiments(
            &self.workspace_id()?,
            &task.id,
            false,
            true,
            &|records, existing, _| {
                let record = records
                    .iter()
                    .find(|record| record.experiment.id == id)
                    .or_else(|| records.iter().find(|record| record.experiment.name == id))
                    .ok_or_else(|| {
                        OrbitError::InvalidInput(format!("no experiment `{id}` in this workspace"))
                    })?;
                if record.experiment.status != "running" {
                    return Err(OrbitError::InvalidInput(
                        "forced assignment requires a running experiment".into(),
                    ));
                }
                // An explicit-crew exclusion yields to the operator's choice.
                if existing.iter().any(|row| {
                    row.experiment == record.experiment.id
                        && row.exclusion_reason.as_deref() != Some(EXPLICIT_CREW_EXCLUSION)
                }) {
                    return Err(OrbitError::InvalidInput(
                        "task already has a sticky assignment or exclusion for this experiment"
                            .into(),
                    ));
                }
                if !occupied_surfaces(records, existing)?.is_disjoint(&experiment_surfaces(record)?)
                {
                    return Err(OrbitError::InvalidInput(
                        "surface_conflict: another experiment already owns this task's surface"
                            .into(),
                    ));
                }
                let selected = record
                    .arms
                    .iter()
                    .find(|candidate| candidate.name == arm && candidate.retired_segment.is_none())
                    .ok_or_else(|| {
                        OrbitError::InvalidInput(format!(
                            "no active arm `{arm}` in experiment `{id}`"
                        ))
                    })?;
                Ok(vec![ExperimentAssignment {
                    experiment: record.experiment.id.clone(),
                    task_id: task.id.clone(),
                    arm: Some(selected.name.clone()),
                    segment: Some(record.experiment.segment),
                    digest: Some(selected.overlay_digest.clone()),
                    assigned_at: Utc::now(),
                    forced: true,
                    exclusion_reason: None,
                }])
            },
        )
    }

    /// Stored assignment readout, including surface-conflict exclusions.
    pub fn task_experiments(&self, task_id: &str) -> Result<Vec<ExperimentAssignment>, OrbitError> {
        self.sqlite_store()?
            .task_experiment_assignments(&self.workspace_id()?, task_id)
    }
}

fn population_matches(filter: &PopulationFilter, task: &Task) -> bool {
    let includes = |values: &[String], value: Option<&str>| {
        values.is_empty() || value.is_some_and(|v| values.iter().any(|candidate| candidate == v))
    };
    includes(
        &filter.complexity,
        task.complexity.as_ref().map(|c| c.as_str()),
    ) && includes(&filter.task_type, Some(&task.task_type.to_string()))
        && includes(
            &filter.origin,
            task.origin.as_ref().map(|origin| origin.as_str()),
        )
        && filter
            .tags_include
            .iter()
            .all(|tag| task.tags.contains(tag))
        && !filter
            .tags_exclude
            .iter()
            .any(|tag| task.tags.contains(tag))
}

fn experiment_surfaces(record: &ExperimentRecord) -> Result<BTreeSet<&'static str>, OrbitError> {
    let mut surfaces = BTreeSet::new();
    // Reserve the experiment's union, including retired arms: drawing control
    // or retiring a treatment must not allow an overlapping experiment in.
    for arm in &record.arms {
        let overlay: ExperimentOverlay = serde_json::from_str(&arm.overlay_json)
            .map_err(|error| OrbitError::Store(format!("invalid experiment overlay: {error}")))?;
        if !overlay.instructions.is_empty() {
            surfaces.insert("instructions");
        }
        if overlay.skills.is_some() {
            surfaces.insert("skills");
        }
        if overlay.tools.is_some() {
            surfaces.insert("tools");
        }
        if overlay.crew.is_some() {
            surfaces.insert("crew");
        }
        if overlay.config.is_some() {
            surfaces.insert("config");
        }
    }
    Ok(surfaces)
}

fn occupied_surfaces(
    records: &[ExperimentRecord],
    assignments: &[ExperimentAssignment],
) -> Result<BTreeSet<&'static str>, OrbitError> {
    let mut occupied = BTreeSet::new();
    for assignment in assignments.iter().filter(|row| row.arm.is_some()) {
        let record = records
            .iter()
            .find(|record| record.experiment.id == assignment.experiment)
            .ok_or_else(|| OrbitError::Store("assigned experiment is missing".into()))?;
        occupied.extend(experiment_surfaces(record)?);
    }
    Ok(occupied)
}
