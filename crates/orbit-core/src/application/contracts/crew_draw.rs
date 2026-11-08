//! What task writes and the crew pools agree on about a crew draw: the
//! candidates a draw weighs and the assignment a task records. The
//! provenance a task records is [`CrewSource`]; a draw's own `source`
//! string stays display provenance for the run, not a task record.

use orbit_types::identity::{
    AutoEffortBasis, AutoEffortChoice, AutoEffortSignal, Crew, ReasoningEffort,
};
use orbit_types::task::{CrewSource, TaskComplexity};
use orbit_types::workflow::{ActivityCrewDraw, ActivityCrewPoolMember};

/// The reason an inspection-requiring activity names when no crew it may
/// draw can inspect [ORB-15792].
pub(crate) const INSPECTION_CREW_UNAVAILABLE: &str = "inspection_crew_unavailable";

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

/// Crew chosen for a task at creation, with the provenance its history entry
/// records [ORB-12717].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CreationCrewAssignment {
    pub(crate) crew: String,
    pub(crate) source: CrewSource,
}

/// One crew a task may be admitted to, with the tickets it holds in the draw.
/// A crew chosen outside a pool — explicit, `task.crew`, or the default chain —
/// is the only candidate and holds a single ticket.
#[derive(Debug, Clone)]
pub(crate) struct CrewCandidate {
    pub(crate) crew: Crew,
    pub(crate) weight: u32,
    /// How a `<crew>@auto` member resolved to `crew` for this draw; `None`
    /// for a member that names its effort (or none) itself.
    pub(crate) auto_effort: Option<AutoEffortChoice>,
}

/// What a `<crew>@auto` pool member resolves against in one draw: the
/// subject's signal, the seed of its exploration roll, and an effort a
/// task-pilot escalation forces. A draw without a subject resolves every
/// `@auto` member to its crew's configured effort and never explores.
#[derive(Debug, Clone, Default)]
pub(crate) struct AutoEffortSubject {
    pub(crate) signal: AutoEffortSignal,
    /// Seeds the exploration roll so a retried draw resolves alike; `None`
    /// never explores.
    pub(crate) seed: Option<String>,
    pub(crate) forced: Option<ReasoningEffort>,
}

/// A selected crew and the exact eligible weights used for this draw.
pub(crate) struct CrewPoolDraw {
    pub(crate) crew: Crew,
    pub(crate) source: String,
    pub(crate) eligible_pool: Vec<ActivityCrewPoolMember>,
    /// The selected member's `@auto` resolution, when it was one.
    pub(crate) auto_effort: Option<AutoEffortChoice>,
}

impl CrewPoolDraw {
    pub(crate) fn into_activity_draw(self) -> ActivityCrewDraw {
        ActivityCrewDraw {
            crew: self.crew.name,
            source: self.source,
            eligible_pool: self.eligible_pool,
            auto_effort: self.auto_effort,
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

/// The provenance note a draw's source carries for an `@auto` member:
/// `auto effort opus@auto -> opus@high (band 2)`, with `explored; policy
/// <reference>` when the exploration share chose another effort.
pub(crate) fn auto_effort_note(choice: &AutoEffortChoice) -> String {
    let basis = match choice.basis {
        AutoEffortBasis::Band => choice
            .band
            .map_or_else(|| "band".to_string(), |band| format!("band {}", band + 1)),
        AutoEffortBasis::Fallback => "configured effort".to_string(),
        AutoEffortBasis::Escalation => "pilot escalation".to_string(),
    };
    if choice.explored {
        format!(
            "auto effort {} -> {} (explored; policy {} by {basis})",
            choice.requested, choice.resolved, choice.policy
        )
    } else {
        format!(
            "auto effort {} -> {} ({basis})",
            choice.requested, choice.resolved
        )
    }
}
