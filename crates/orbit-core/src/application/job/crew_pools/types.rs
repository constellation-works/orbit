use std::collections::BTreeMap;

use orbit_config::CrewPoolEntry;
use orbit_types::task::TaskComplexity;
use serde::{Deserialize, Serialize};

pub(super) const POOLS_KEY: &str = "auto_crew_pools";
/// The pipelines complexity-pool routing applies to: the two workspace
/// coordinators and the task-carrying delivery pipelines [ORB-12606].
///
/// Policy is captured by whichever of these is admitted without a
/// policy-bearing parent, so an ordinary `run ship` routes a crew-less task
/// exactly as a drain does. System, review and preparation jobs are absent
/// from the list and keep their own crew selection.
pub(super) const POLICY_PIPELINES: [&str; 6] = [
    "workspace_auto_pipeline",
    "workspace_ship_pipeline",
    "task_auto_pipeline",
    "task_gate_pipeline",
    "task_local_pipeline",
    "task_pr_pipeline",
];
pub(super) const SELECTION_KEY: &str = "crew_selection";
pub(super) const COMPLEXITIES: [TaskComplexity; 4] = [
    TaskComplexity::Low,
    TaskComplexity::Medium,
    TaskComplexity::Hard,
    TaskComplexity::XHard,
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CapturedCrewPool {
    /// Weighted members. A pool captured before weights existed is a plain
    /// name list, which [`CrewPoolEntry`] still deserialises at weight 1.
    pub(super) crews: Vec<CrewPoolEntry>,
    pub(super) source: String,
}

pub(crate) type CapturedCrewPools = BTreeMap<String, CapturedCrewPool>;
