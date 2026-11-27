//! Each experiment's enrollment so far, for the Analytics › Experiments list:
//! the definition as `list` shows it, with every arm's unforced enrolled
//! tasks overall and in the current segment. It counts assignment rows only,
//! so it reads no outcome records; the readout counts settled outcomes.

use orbit_common::OrbitError;
use serde::Serialize;

use super::view::ExperimentView;
use crate::OrbitRuntime;

/// One experiment with its enrollment.
#[derive(Debug, Clone, Serialize)]
pub struct ExperimentProgress {
    #[serde(flatten)]
    pub experiment: ExperimentView,
    /// One row per arm, in the experiment's arm order.
    pub enrollment: Vec<ArmEnrollment>,
    /// Forced assignments, which no estimate counts.
    pub forced: usize,
    /// Tasks excluded at admission because another experiment owned a
    /// surface.
    pub excluded: usize,
}

/// One arm's unforced enrolled tasks.
#[derive(Debug, Clone, Serialize)]
pub struct ArmEnrollment {
    pub arm: String,
    pub enrolled: usize,
    /// Enrolled in the experiment's current segment.
    pub segment_enrolled: usize,
}

/// The workspace's experiments, oldest first, each with its enrollment.
pub(crate) fn experiment_progress(
    rt: &OrbitRuntime,
) -> Result<Vec<ExperimentProgress>, OrbitError> {
    let workspace = rt.workspace_id()?;
    let store = rt.sqlite_store()?;
    let snapshot = store.read()?;
    snapshot
        .experiment()
        .list(&workspace, None)?
        .into_iter()
        .map(|record| {
            let rows = snapshot
                .experiment_assignment()
                .list_for_experiment(&workspace, &record.experiment.id)?;
            let segment = record.experiment.segment;
            let enrollment = record
                .arms
                .iter()
                .map(|arm| {
                    let mine = rows
                        .iter()
                        .filter(|row| !row.forced && row.arm.as_deref() == Some(&arm.name));
                    ArmEnrollment {
                        arm: arm.name.clone(),
                        enrolled: mine.clone().count(),
                        segment_enrolled: mine.filter(|row| row.segment == Some(segment)).count(),
                    }
                })
                .collect();
            let forced = rows.iter().filter(|row| row.forced).count();
            let excluded = rows
                .iter()
                .filter(|row| row.arm.is_none() && row.exclusion_reason.is_some())
                .count();
            let experiment = ExperimentView::from_record(record)?;

            Ok(ExperimentProgress {
                experiment,
                enrollment,
                forced,
                excluded,
            })
        })
        .collect()
}
