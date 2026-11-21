//! Pull-drain evidence shared by full run-detail surfaces.

use orbit_common::OrbitError;
use orbit_store::contracts::DrainLeafOccupancy;
use orbit_types::workflow::{PipelineState, PullBreaker};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::contracts::{
    DrainClaimedLeaf, PullCrewWindow, PullLeafClaim, RefusedPullSettlement,
};
use crate::application::job::JobDrainPort;

/// Whether the run has a registered runtime capable of reading live evidence.
pub enum RunPullDrainSource<'a> {
    Registered(&'a OrbitRuntime),
    /// Stored runs retain their checkpoint breaker, but have no live sections.
    Recorded,
}

/// Why one section was omitted, for the surface's diagnostic channel.
pub struct RunPullDrainWarning {
    pub section: &'static str,
    pub error: OrbitError,
}

/// The six pull-drain sections of a full run detail, independent of its profile.
/// Unreadable optional sections are absent and unreadable lists are empty.
/// An unreadable occupancy remains unknown, never a claimed empty budget.
pub struct RunPullDrainEvidence {
    pub pull_claim: Option<PullLeafClaim>,
    pub claimed_leaves: Vec<DrainClaimedLeaf>,
    pub refused_settlements: Vec<RefusedPullSettlement>,
    pub crew_window: Option<PullCrewWindow>,
    pull_occupancy: Option<DrainLeafOccupancy>,
    pub pull_breaker: Option<PullBreaker>,
    pub warnings: Vec<RunPullDrainWarning>,
}

impl RunPullDrainEvidence {
    /// Render the same sibling sections into a CLI, HTTP or tool run detail.
    /// Diagnostics stay out of the wire shape and are rendered by the surface.
    pub fn append_pull_drain_sections(&self, value: &mut Value) {
        value["pull_claim"] = json!(self.pull_claim);
        value["claimed_leaves"] = json!(self.claimed_leaves);
        value["refused_settlements"] = json!(self.refused_settlements);
        value["crew_window"] = json!(self.crew_window);
        value["pull_occupancy"] = json!(self.pull_occupancy.as_ref().map(|occupancy| {
            json!({ "occupied": occupancy.occupied, "reviewing": occupancy.reviewing })
        }));
        value["pull_breaker"] = json!(self.pull_breaker);
    }
}

/// Gather a run's pull-drain detail once, with one warn-and-empty policy.
/// The checkpoint is supplied by the caller so the breaker agrees with the
/// rest of that response. Recorded-only reads never consult a different
/// workspace's live admissions or capacity.
pub fn read_run_pull_drain_evidence(
    source: RunPullDrainSource<'_>,
    run_id: &str,
    state: Option<&PipelineState>,
) -> RunPullDrainEvidence {
    let mut evidence = RunPullDrainEvidence {
        pull_claim: None,
        claimed_leaves: Vec::new(),
        refused_settlements: Vec::new(),
        crew_window: None,
        pull_occupancy: None,
        pull_breaker: state.and_then(|state| state.pull_drain.pull_breaker.clone()),
        warnings: Vec::new(),
    };
    if let RunPullDrainSource::Registered(runtime) = source {
        evidence.pull_claim = pull_section_or_empty(
            runtime.run_pull_claim(run_id),
            "pull claim",
            &mut evidence.warnings,
        );
        evidence.claimed_leaves = pull_section_or_empty(
            runtime.drain_claimed_leaves(run_id),
            "claimed leaves",
            &mut evidence.warnings,
        );
        evidence.refused_settlements = pull_section_or_empty(
            runtime.drain_refused_settlements(run_id),
            "refused settlements",
            &mut evidence.warnings,
        );
        evidence.crew_window = pull_section_or_empty(
            runtime.drain_crew_window(run_id),
            "crew window",
            &mut evidence.warnings,
        );
        evidence.pull_occupancy = pull_section_or_empty(
            runtime.drain_pull_occupancy(run_id),
            "pull occupancy",
            &mut evidence.warnings,
        );
    }
    evidence
}

fn pull_section_or_empty<T: Default>(
    result: Result<T, OrbitError>,
    section: &'static str,
    warnings: &mut Vec<RunPullDrainWarning>,
) -> T {
    match result {
        Ok(value) => value,
        Err(error) => {
            warnings.push(RunPullDrainWarning { section, error });
            T::default()
        }
    }
}
