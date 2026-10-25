//! What task writes and the crew pools agree on about a crew draw: the
//! candidates a draw weighs, the assignment and provenance a task records,
//! and the provenance prefixes and markers that classify it.

use orbit_types::identity::Crew;
use orbit_types::task::TaskComplexity;
use orbit_types::workflow::{ActivityCrewDraw, ActivityCrewPoolMember};

/// Prefix of the provenance a task records when its crew was drawn from its
/// purpose's pool, as `purpose:<purpose>`.
pub(crate) const PURPOSE_SOURCE_PREFIX: &str = "purpose:";

/// Prefix of the task comment that records a provider-limit reassignment of
/// an auto-task's explicit crew [ORB-17638].
pub(crate) const PROVIDER_LIMIT_REASSIGNMENT_PREFIX: &str = "Provider-limit reassignment:";

/// Whether `tags` mark a task minted by an auto-task (`auto-task:<name>`).
pub(crate) fn auto_task_minted(tags: &[String]) -> bool {
    tags.iter().any(|tag| {
        tag.strip_prefix("auto-task:")
            .is_some_and(|name| !name.is_empty())
    })
}

/// Whether `source` is a system assignment — a purpose pool, a complexity
/// pool, or the default — rather than an explicit pin or a legacy crew with
/// no assignment evidence.
pub(crate) fn automatic_crew_source(source: &str) -> bool {
    source == "default" || source.starts_with("pool:") || source.starts_with(PURPOSE_SOURCE_PREFIX)
}

/// Crew chosen for a task at creation, with the provenance its history entry
/// records: `explicit`, `pool:<complexity>`, or `default` [ORB-12717].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CreationCrewAssignment {
    pub(crate) crew: String,
    pub(crate) source: String,
}

/// One crew a task may be admitted to, with the tickets it holds in the draw.
/// A crew chosen outside a pool — explicit, `task.crew`, or the default chain —
/// is the only candidate and holds a single ticket.
#[derive(Debug, Clone)]
pub(crate) struct CrewCandidate {
    pub(crate) crew: Crew,
    pub(crate) weight: u32,
}

/// A selected crew and the exact eligible weights used for this draw.
pub(crate) struct CrewPoolDraw {
    pub(crate) crew: Crew,
    pub(crate) source: String,
    pub(crate) eligible_pool: Vec<ActivityCrewPoolMember>,
}

impl CrewPoolDraw {
    pub(crate) fn into_activity_draw(self) -> ActivityCrewDraw {
        ActivityCrewDraw {
            crew: self.crew.name,
            source: self.source,
            eligible_pool: self.eligible_pool,
        }
    }
}

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
