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
use orbit_types::identity::Crew;
use orbit_types::task::{Task, TaskComplexity};
use orbit_types::workflow::{CREW_PURPOSE_TAG_PREFIX, CrewPurpose};

use crate::OrbitRuntime;
use crate::application::contracts::{
    CI_FAILURE_TAG, CODE_SCANNING_TAG, CrewCandidate, DEPENDABOT_TAG, PURPOSE_SOURCE_PREFIX,
    RECONCILIATION_FOLLOW_UP_TAG_PREFIX,
};
use crate::application::task::BASELINE_RED_REPAIR_TAG;

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
        CI_FAILURE_TAG | DEPENDABOT_TAG | CODE_SCANNING_TAG => Some(CrewPurpose::SweepTasks),
        _ if tag.starts_with(RECONCILIATION_FOLLOW_UP_TAG_PREFIX) => Some(CrewPurpose::SweepTasks),
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
        let enabled = pool
            .entries
            .iter()
            .filter(|entry| {
                orbit_types::identity::find_crew(&entry.name, registry)
                    .is_some_and(|crew| crew.enabled)
            })
            .map(|entry| {
                Ok(CrewCandidate {
                    crew: self.resolve_crew_for_task(Some(&entry.name), None)?,
                    weight: entry.weight,
                })
            })
            .collect::<Result<Vec<_>, OrbitError>>()?;
        if enabled.is_empty() {
            if !pool.entries.is_empty() {
                tracing::info!(
                    pool = %key,
                    "every crew in the purpose pool is disabled; the purpose keeps its fallback route",
                );
            }
            return Ok(None);
        }
        let declared = self.declared_executor_host_os()?;
        let candidates = enabled
            .into_iter()
            .filter(|candidate| self.crew_runs_on_host(&candidate.crew, &declared))
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            tracing::info!(
                pool = %key,
                "every crew in the purpose pool is excluded by its executor host_os; the purpose keeps its fallback route",
            );
            return Ok(None);
        }
        if !inspection {
            return Ok(Some((candidates, key.to_string())));
        }
        let (capable, unable) = self.partition_inspecting(
            candidates
                .into_iter()
                .filter(|candidate| candidate.weight > 0)
                .collect(),
        )?;
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

    /// The enabled members of the purpose pool `task` was assigned from, when
    /// that assignment is automatic (`purpose:<purpose>`).
    ///
    /// `None` when the crew is an explicit pin, a complexity-pool or default
    /// assignment, or the purpose pool is unset, empty or wholly disabled.
    /// A hold or a usage limit retries this pool, weights included, before the
    /// complexity-pool fallback. An explicit crew never reaches it.
    pub(crate) fn purpose_assignment_candidates(
        &self,
        task: &Task,
    ) -> Result<Option<(Vec<CrewCandidate>, String)>, OrbitError> {
        let Some(purpose) = self
            .task_crew_source(task)?
            .as_deref()
            .and_then(|source| source.strip_prefix(PURPOSE_SOURCE_PREFIX))
            .and_then(CrewPurpose::parse)
        else {
            return Ok(None);
        };
        self.purpose_pool_candidates(purpose, false)
    }

    /// The image-capable candidates of one visual-review pool
    /// (§9.4): members whose executor declares `image_input`, whose
    /// crew does not set `vision = false`, and who hold no visual
    /// (canary) hold. Each survivor keeps its executor's declaration,
    /// whose `max_images` the budget also reads.
    pub(crate) fn visual_image_candidates(
        &self,
        candidates: Vec<CrewCandidate>,
    ) -> Result<Vec<(CrewCandidate, orbit_types::workflow::ExecutorImageInput)>, OrbitError> {
        let mut capable = Vec::new();
        for candidate in candidates {
            if candidate.weight == 0 {
                continue;
            }
            let Some(declared) = self.can_take_images(&candidate.crew)? else {
                continue;
            };
            // A crew held after consecutive canary misses is passed
            // over while the hold stands (§9.3); a hold lookup that
            // cannot run does not disqualify it.
            if matches!(self.visual_hold(&candidate.crew.name), Ok(Some(_))) {
                continue;
            }
            capable.push((candidate, declared));
        }
        Ok(capable)
    }

    /// The pool visual review draws from before the image filter:
    /// `workflow.purpose_crews.visual_review` when configured, else
    /// the task's complexity pool (`tier`), as §9.4 names, with the
    /// draw source. Empty when neither pool has an enabled member.
    pub(crate) fn visual_review_pool(
        &self,
        tier: TaskComplexity,
    ) -> Result<(Vec<CrewCandidate>, String), OrbitError> {
        if let Some((candidates, source)) =
            self.purpose_pool_candidates(CrewPurpose::VisualReview, false)?
        {
            return Ok((candidates, source));
        }
        let source = super::draw::complexity_crews_key(tier);
        Ok((
            self.configured_complexity_pool(tier)?.unwrap_or_default(),
            source,
        ))
    }

    /// Draw one crew from `candidates`, seeded by task and candidate
    /// so a retried step draws the same crew (§9.4). `None` for an
    /// empty pool.
    pub(crate) fn visual_seeded_draw(
        &self,
        candidates: Vec<CrewCandidate>,
        source: &str,
        seed: &str,
    ) -> Result<Option<Crew>, OrbitError> {
        if candidates.is_empty() {
            return Ok(None);
        }
        let mut ticket = super::draw::seeded_crew_ticket(seed.to_string());
        Ok(Some(
            self.draw_crew_pool(candidates, source, &mut ticket)?.crew,
        ))
    }
}
