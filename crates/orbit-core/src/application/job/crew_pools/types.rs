use std::collections::BTreeMap;

use orbit_config::CrewPoolEntry;
use orbit_types::identity::Crew;
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

/// Crew chosen for a task at creation, with the provenance its history entry
/// records: `explicit`, `pool:<complexity>`, or `default` [ORB-12717].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CreationCrewAssignment {
    pub(crate) crew: String,
    pub(crate) source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CapturedCrewPool {
    /// Weighted members. A pool captured before weights existed is a plain
    /// name list, which [`CrewPoolEntry`] still deserialises at weight 1.
    pub(super) crews: Vec<CrewPoolEntry>,
    pub(super) source: String,
}

/// One crew a task may be admitted to, with the tickets it holds in the draw.
/// A crew chosen outside a pool — explicit, `task.crew`, or the default chain —
/// is the only candidate and holds a single ticket.
#[derive(Debug, Clone)]
pub(crate) struct CrewCandidate {
    pub(crate) crew: Crew,
    pub(crate) weight: u32,
}

pub(crate) type CapturedCrewPools = BTreeMap<String, CapturedCrewPool>;

/// The members of one complexity pool that can inspect pinned source natively
/// [ORB-15792]: the first tier, from the one asked for upward, whose enabled
/// pool has such a member with a positive weight.
pub(crate) struct InspectionPool {
    /// The tier the members were drawn from.
    pub(crate) tier: TaskComplexity,
    pub(crate) candidates: Vec<CrewCandidate>,
    /// Enabled members passed over, in this tier and the ones below it,
    /// because their provider has no native inspection surface.
    pub(crate) skipped: Vec<String>,
}
