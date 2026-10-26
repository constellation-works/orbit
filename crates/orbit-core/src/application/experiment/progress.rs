//! Each experiment's enrollment so far, for the Analytics › Experiments list:
//! the definition as `list` shows it, with every arm's unforced enrolled
//! tasks overall and in the current segment. It counts assignment rows only,
//! so it reads no outcome records; the readout counts settled outcomes.

use orbit_common::OrbitError;
use orbit_store::contracts::ExperimentStoreBackend as _;
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

impl OrbitRuntime {
    /// The workspace's experiments, oldest first, each with its enrollment.
    pub fn experiment_progress(&self) -> Result<Vec<ExperimentProgress>, OrbitError> {
        let workspace = self.workspace_id()?;
        let store = self.sqlite_store()?;
        store
            .experiment_records(&workspace, None)?
            .into_iter()
            .map(|record| {
                let rows = store.experiment_assignments(&workspace, &record.experiment.id)?;
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
                            segment_enrolled: mine
                                .filter(|row| row.segment == Some(segment))
                                .count(),
                        }
                    })
                    .collect();
                Ok(ExperimentProgress {
                    enrollment,
                    forced: rows.iter().filter(|row| row.forced).count(),
                    excluded: rows
                        .iter()
                        .filter(|row| row.arm.is_none() && row.exclusion_reason.is_some())
                        .count(),
                    experiment: ExperimentView::from_record(record)?,
                })
            })
            .collect()
    }
}
