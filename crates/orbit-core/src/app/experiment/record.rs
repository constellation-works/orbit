//! The experiment write rules, run inside one `Store::write` transaction so a
//! check and the write it guards cannot interleave with another admission or
//! lifecycle change.

use orbit_common::OrbitError;
use orbit_store::Tx;
use orbit_store::contract::{
    ExperimentAssignment, ExperimentRecord, ExperimentWrite, ExperimentWriteOutcome,
};

/// Pure assignment decision inside the store transaction, given the
/// workspace's experiments, the task's existing rows and whether the task was
/// already admitted. Existing rows cannot be replaced, except that a forced
/// assignment may replace an exclusion. `admitted` includes prior admissions
/// that matched no population; a marked admission never reaches the decision
/// once the task is admitted.
pub type ExperimentAssignmentDecision<'a> = dyn Fn(
        &[ExperimentRecord],
        &[ExperimentAssignment],
        bool,
    ) -> Result<Vec<ExperimentAssignment>, OrbitError>
    + 'a;

/// Write the assignments `decide` returns for one task and, when
/// `mark_admitted`, mark the task admitted; returns the task's rows after.
///
/// `experiments_enabled` is the caller's current effective enablement. While
/// it is false, a marked admission writes nothing and returns existing rows.
/// Once an enabled marked admission has run, later marked admissions return
/// the existing rows unchanged, even when that first admission matched none.
pub fn assign_experiments(
    tx: &Tx<'_>,
    workspace_id: &str,
    task_id: &str,
    mark_admitted: bool,
    experiments_enabled: bool,
    decide: &ExperimentAssignmentDecision<'_>,
) -> Result<Vec<ExperimentAssignment>, OrbitError> {
    let assignments = tx.experiment_assignment();
    let admitted = assignments.admitted(workspace_id, task_id)?;
    let existing = assignments.list_for_task(workspace_id, task_id)?;
    // An enabled admission is final, including one that matched no
    // population: a later edit or retry cannot move the task into a cohort.
    // Migration v70 is the one-time exception: it reopens every
    // admission-only row because stored rows cannot distinguish an enabled
    // no-match admission from one written while disabled.
    if mark_admitted && admitted {
        return Ok(existing);
    }
    // Disabled admission writes nothing, so it cannot mark the task admitted
    // and exclude it from a later enabled admission.
    if mark_admitted && !experiments_enabled {
        return Ok(existing);
    }
    let records = tx.experiment().list(workspace_id, None)?;
    for assignment in decide(&records, &existing, admitted)? {
        if assignment.task_id != task_id {
            return Err(OrbitError::Store(
                "experiment assignment task mismatch".into(),
            ));
        }
        if !assignments.put(workspace_id, &assignment)? {
            return Err(OrbitError::Store(
                "experiment assignment already exists for this task".into(),
            ));
        }
    }
    if mark_admitted {
        assignments.put_admission(workspace_id, task_id)?;
    }
    assignments.list_for_task(workspace_id, task_id)
}

/// Apply `write` unless the stored revision moved, the name is taken by
/// another experiment, or the running limit is reached.
pub fn write_experiment(
    tx: &Tx<'_>,
    write: &ExperimentWrite,
) -> Result<ExperimentWriteOutcome, OrbitError> {
    let experiment = &write.experiment;
    if write.event.seq != experiment.revision {
        return Err(OrbitError::Store(format!(
            "experiment event seq {} does not match revision {}",
            write.event.seq, experiment.revision
        )));
    }
    let records = tx.experiment();
    if records.revision(&experiment.workspace_id, &experiment.id)? != write.expected_revision {
        return Ok(ExperimentWriteOutcome::Stale);
    }
    if records.name_exists(&experiment.workspace_id, &experiment.name, &experiment.id)? {
        return Ok(ExperimentWriteOutcome::NameTaken);
    }
    if let Some(limit) = write.running_limit {
        let running = records.count_running(&experiment.workspace_id, &experiment.id)?;
        if running >= limit {
            return Ok(ExperimentWriteOutcome::RunningLimitReached { running });
        }
    }
    records.put(experiment)?;
    if let Some(arms) = &write.arms {
        records.put_arms(experiment, arms)?;
    }
    records.put_event(experiment, &write.event)?;
    Ok(ExperimentWriteOutcome::Written)
}

/// Delete one experiment with its arms, events and assignment rows, unless
/// its stored revision is no longer `expected_revision`. Returns whether it
/// was deleted.
pub(crate) fn delete_experiment(
    tx: &Tx<'_>,
    workspace_id: &str,
    experiment_id: &str,
    expected_revision: u32,
) -> Result<bool, OrbitError> {
    let records = tx.experiment();
    if records.revision(workspace_id, experiment_id)? != Some(expected_revision) {
        return Ok(false);
    }
    records.delete(workspace_id, experiment_id)?;
    Ok(true)
}
