//! Purpose crew pools: each purpose of work may name a weighted pool of its
//! own (`workflow.purpose_crews.<purpose>`), drawn ahead of the complexity
//! pools and behind an explicit crew.
//!
//! An activity names its purpose through its `crew_config_key`; a task's
//! purpose comes from its tags: a `crew-purpose:<purpose>` tag, else the tag
//! the system filed it with. A purpose whose pool is unset, empty or wholly
//! disabled keeps the route it had before purposes existed.

use orbit_common::OrbitError;
use orbit_config::canonical_crew_pool;
use orbit_types::task::TaskComplexity;
use orbit_types::workflow::{CREW_PURPOSE_TAG_PREFIX, CrewPurpose};

use crate::OrbitRuntime;
use crate::application::review::reconciliation::FOLLOW_UP_TAG_PREFIX;
use crate::application::sweep_filing::ci_failure::filing::CI_FAILURE_TAG;
use crate::application::sweep_filing::dependabot::filing::{CODE_TAG, DEPENDABOT_TAG};
use crate::application::task::BASELINE_RED_REPAIR_TAG;

use super::draw::can_inspect;
use super::types::CrewCandidate;

/// Prefix of the provenance a task records when its crew was drawn from its
/// purpose's pool, as `purpose:<purpose>`.
pub(crate) const PURPOSE_SOURCE_PREFIX: &str = "purpose:";

/// The purpose `tags` name for a task's crew: an explicit
/// `crew-purpose:<purpose>` tag naming a `workflow.purpose_crews` purpose,
/// else the system tag the task was filed with.
pub(crate) fn task_crew_purpose(tags: &[String]) -> Option<CrewPurpose> {
    tags.iter()
        .filter_map(|tag| tag.strip_prefix(CREW_PURPOSE_TAG_PREFIX))
        .filter_map(CrewPurpose::parse)
        .find(|purpose| purpose.has_purpose_key())
        .or_else(|| tags.iter().find_map(|tag| system_tag_purpose(tag)))
}

fn system_tag_purpose(tag: &str) -> Option<CrewPurpose> {
    match tag {
        BASELINE_RED_REPAIR_TAG => Some(CrewPurpose::BaselineRedRepair),
        CI_FAILURE_TAG => Some(CrewPurpose::CiFailureSweep),
        DEPENDABOT_TAG | CODE_TAG => Some(CrewPurpose::Dependabot),
        _ if tag.starts_with(FOLLOW_UP_TAG_PREFIX) => Some(CrewPurpose::MergedHeadRepair),
        _ => None,
    }
}

/// The complexity pool an activity purpose draws from while its own pool is
/// unset: the pool its activities named before purposes existed. `None` for
/// a purpose no activity dispatches under a `workflow.purpose_crews` key.
pub(super) fn activity_fallback_tier(purpose: CrewPurpose) -> Option<TaskComplexity> {
    match purpose {
        CrewPurpose::TaskPilot | CrewPurpose::AutoFix => Some(TaskComplexity::Low),
        CrewPurpose::FailureRecovery => Some(TaskComplexity::Medium),
        _ => None,
    }
}

impl OrbitRuntime {
    /// The enabled members of `purpose`'s `workflow.purpose_crews` pool, with
    /// the source a draw over them records.
    ///
    /// `None` when the pool is unset, empty or wholly disabled, which keeps the
    /// purpose on its fallback route. With `inspection`, only members whose
    /// provider inspects source natively and that hold a ticket are kept; a
    /// pool with none of those is `None` too, so the fallback's own inspection
    /// rules apply.
    pub(crate) fn purpose_pool_candidates(
        &self,
        purpose: CrewPurpose,
        inspection: bool,
    ) -> Result<Option<(Vec<CrewCandidate>, String)>, OrbitError> {
        if !purpose.has_purpose_key() {
            return Ok(None);
        }
        let key = purpose.config_key();
        let settings = self.context.settings();
        let registry = settings.crews();
        let pool = canonical_crew_pool(settings.purpose_crews().pool(purpose), registry, key)?;
        let candidates = pool
            .entries
            .iter()
            .filter(|entry| registry.get(&entry.name).is_some_and(|crew| crew.enabled))
            .map(|entry| {
                Ok(CrewCandidate {
                    crew: self.resolve_crew_for_task(Some(&entry.name), None)?,
                    weight: entry.weight,
                })
            })
            .collect::<Result<Vec<_>, OrbitError>>()?;
        if candidates.is_empty() {
            if !pool.entries.is_empty() {
                tracing::info!(
                    pool = %key,
                    "every crew in the purpose pool is disabled; the purpose keeps its fallback route",
                );
            }
            return Ok(None);
        }
        if !inspection {
            return Ok(Some((candidates, key.to_string())));
        }
        let (capable, unable): (Vec<_>, Vec<_>) = candidates
            .into_iter()
            .filter(|candidate| candidate.weight > 0)
            .partition(|candidate| can_inspect(&candidate.crew));
        if capable.is_empty() {
            tracing::warn!(
                pool = %key,
                "no crew in the purpose pool can inspect natively; the purpose keeps its fallback route",
            );
            return Ok(None);
        }
        let source = if unable.is_empty() {
            format!("{key}; native inspection")
        } else {
            let skipped = unable
                .iter()
                .map(|candidate| candidate.crew.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!("{key}; native inspection, skipped: {skipped}")
        };
        Ok(Some((capable, source)))
    }
}
