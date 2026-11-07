//! What task writes and the crew pools agree on about a crew draw: the
//! candidates a draw weighs, the assignment and provenance a task records,
//! and the provenance prefixes and markers that classify it.

use orbit_types::identity::{
    AutoEffortBasis, AutoEffortChoice, AutoEffortSignal, Crew, ReasoningEffort,
};
use orbit_types::task::TaskComplexity;
use orbit_types::workflow::{ActivityCrewDraw, ActivityCrewPoolMember};

/// The reason an inspection-requiring activity names when no crew it may
/// draw can inspect [ORB-15792].
pub(crate) const INSPECTION_CREW_UNAVAILABLE: &str = "inspection_crew_unavailable";

/// Prefix of the provenance a task records when its crew was drawn from its
/// purpose's pool, as `purpose:<purpose>`.
pub(crate) const PURPOSE_SOURCE_PREFIX: &str = "purpose:";

/// Prefix of the provenance a task records when its crew was drawn from its
/// type's pool, as `type:<type>/<tier>`
/// (`workflow.type_complexity_crews.<type>.<tier>`).
pub(crate) const TYPE_SOURCE_PREFIX: &str = "type:";

/// The provenance a draw from `task_type`'s pool at `tier` records.
pub(crate) fn type_crew_source(task_type: &str, tier: TaskComplexity) -> String {
    format!("{TYPE_SOURCE_PREFIX}{task_type}/{tier}")
}

/// The type and tier a `type:<type>/<tier>` source names, or `None` for any
/// other source.
pub(crate) fn parse_type_crew_source(source: &str) -> Option<(&str, TaskComplexity)> {
    let (task_type, tier) = source.strip_prefix(TYPE_SOURCE_PREFIX)?.rsplit_once('/')?;
    let tier = tier.parse::<TaskComplexity>().ok()?;
    (!task_type.is_empty() && tier.is_assessed()).then_some((task_type, tier))
}

/// Prefix of the provenance admission writes back when an experiment arm's
/// crew overlay chose the crew that runs, as `experiment:<id>/<arm>`.
pub(crate) const EXPERIMENT_SOURCE_PREFIX: &str = "experiment:";

/// Prefix of the provenance admission writes back when it ran a task on a
/// crew other than the one recorded, as `redraw:<reason>`: a provider hold
/// or limit, a pool draw, an explicit run crew, or a claimed leaf's own draw.
pub(crate) const REDRAW_SOURCE_PREFIX: &str = "redraw:";

/// The provenance the one-time backfill records on a task whose crew it set
/// from the task's latest delivery run.
pub(crate) const BACKFILL_CREW_SOURCE: &str = "backfill:admission";

/// The provenance an experiment arm's crew overlay writes back.
pub(crate) fn experiment_crew_source(experiment: &str, arm: &str) -> String {
    format!("{EXPERIMENT_SOURCE_PREFIX}{experiment}/{arm}")
}

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

/// Whether `source` is a system assignment — a purpose pool, a type pool, a
/// complexity pool, the default, or an admission write-back — rather than an
/// explicit pin or a legacy crew with no assignment evidence.
pub(crate) fn automatic_crew_source(source: &str) -> bool {
    source == "default"
        || source.starts_with("pool:")
        || source.starts_with(PURPOSE_SOURCE_PREFIX)
        || source.starts_with(TYPE_SOURCE_PREFIX)
        || admission_crew_source(source)
}

/// Whether `source` records a crew admission wrote back for one delivery
/// ([`EXPERIMENT_SOURCE_PREFIX`], [`REDRAW_SOURCE_PREFIX`],
/// [`BACKFILL_CREW_SOURCE`]). Such a crew is the latest delivery's, not a
/// standing assignment: the next admission draws the task's pool again
/// instead of keeping it.
pub(crate) fn admission_crew_source(source: &str) -> bool {
    source.starts_with(EXPERIMENT_SOURCE_PREFIX)
        || source.starts_with(REDRAW_SOURCE_PREFIX)
        || source == BACKFILL_CREW_SOURCE
}

/// Crew chosen for a task at creation, with the provenance its history entry
/// records: `explicit`, `purpose:<purpose>`, `type:<type>/<tier>`,
/// `pool:<complexity>`, or `default` [ORB-12717].
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
