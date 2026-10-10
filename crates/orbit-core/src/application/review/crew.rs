//! The reviewer crew `operation.review_crew` names [ORB-15195].
//!
//! The setting is one crew or a pool written like the complexity pools
//! (`name` or `name:weight`). Each review chooses one member:
//!
//! 1. a member holding no ticket, or one this host cannot run (undefined or
//!    disabled), drops out;
//! 2. a member whose provider is at its usage limit here drops out, unless
//!    every member is;
//! 3. a member that implemented the reviewed work drops out, unless every
//!    remaining member did, so the review stays independent wherever the pool
//!    allows it;
//! 4. one of the rest is drawn by weight.
//!
//! A single crew is a one-member pool, so it is chosen whatever its state and
//! the caller's refusal names it exactly as before pools existed. Every caller
//! seeds the draw from what it reviews, so a retried selection for the same
//! work comes out the same while the pool's state is unchanged.

use std::collections::BTreeSet;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_config::{REVIEW_CREW_KEY, canonical_crew_pool};
use orbit_types::identity::Crew;
use orbit_types::workflow::ReviewCrewPoolMember;

use crate::OrbitRuntime;
use crate::application::job::crew_pools::{CrewCandidate, weighted_draw};

/// How each review picks from a pool, as `orbit config show` and `orbit
/// doctor` state it.
pub(crate) const REVIEW_CREW_SELECTION_RULE: &str = "each review draws one crew by weight, \
     preferring one that did not implement the reviewed work and skipping disabled or \
     provider-limited crews";

/// Provenance of a crew drawn from a pool configured in `layer`.
pub(crate) fn pool_source(layer: &str) -> String {
    format!("{layer} pool")
}

impl OrbitRuntime {
    /// `operation.review_crew` as canonical pool members, in the shape a
    /// review admission captures; empty when unset.
    pub(crate) fn review_crew_pool_members(&self) -> Result<Vec<ReviewCrewPoolMember>, OrbitError> {
        configured_pool_members(self, &self.operation_policy().review_crew.value)
    }

    /// The members this host can run, each with its tickets: step 1. With
    /// none left, the refusal of the first member that holds a ticket.
    pub(crate) fn review_crew_candidates(
        &self,
        pool: &[ReviewCrewPoolMember],
    ) -> Result<Vec<CrewCandidate>, OrbitError> {
        let mut candidates = Vec::new();
        let mut refusal = None;
        for member in pool.iter().filter(|member| member.weight > 0) {
            match self.resolve_crew_for_task(Some(&member.name), None) {
                Ok(crew) => candidates.push(CrewCandidate {
                    crew,
                    weight: member.weight,
                }),
                Err(error) => {
                    refusal.get_or_insert(error);
                }
            }
        }
        if candidates.is_empty() {
            return Err(refusal.unwrap_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "{REVIEW_CREW_KEY} gives no crew a weight above 0"
                ))
            }));
        }
        Ok(candidates)
    }

    /// Steps 2 to 4 over candidates [`Self::review_crew_candidates`] left.
    pub(crate) fn draw_review_crew(
        &self,
        candidates: Vec<CrewCandidate>,
        implementers: &BTreeSet<String>,
        random: &mut impl FnMut() -> Result<u64, OrbitError>,
    ) -> Result<Crew, OrbitError> {
        let gate = self.provider_limit_gate(Utc::now());
        let candidates = prefer(candidates, |candidate| {
            gate.limit_for(&candidate.crew).is_none()
        });
        let candidates = prefer(candidates, |candidate| {
            !implementers.contains(&candidate.crew.name)
        });
        weighted_draw(&candidates, REVIEW_CREW_KEY, random).map(|chosen| chosen.crew.clone())
    }

    /// One reviewer from the configured pool for work `implementers` did,
    /// or `None` when `operation.review_crew` is unset.
    pub(crate) fn select_review_crew(
        &self,
        implementers: &BTreeSet<String>,
        random: &mut impl FnMut() -> Result<u64, OrbitError>,
    ) -> Result<Option<Crew>, OrbitError> {
        let pool = self.review_crew_pool_members()?;
        if pool.is_empty() {
            return Ok(None);
        }
        let candidates = self.review_crew_candidates(&pool)?;
        self.draw_review_crew(candidates, implementers, random)
            .map(Some)
    }
}

/// `operation.review_crew` entries as canonical, weighed members. The value
/// was admitted at load, so this re-reads a pool, never a new grammar.
pub(crate) fn configured_pool_members(
    runtime: &OrbitRuntime,
    entries: &[String],
) -> Result<Vec<ReviewCrewPoolMember>, OrbitError> {
    let pool = canonical_crew_pool(entries, runtime.context.settings().crews(), REVIEW_CREW_KEY)?;
    Ok(pool
        .entries
        .into_iter()
        .map(|entry| ReviewCrewPoolMember {
            name: entry.name,
            weight: entry.weight,
        })
        .collect())
}

/// Keep the candidates `keep` accepts, or all of them when it accepts none.
fn prefer(
    candidates: Vec<CrewCandidate>,
    keep: impl Fn(&CrewCandidate) -> bool,
) -> Vec<CrewCandidate> {
    if candidates.iter().any(&keep) {
        candidates
            .into_iter()
            .filter(|candidate| keep(candidate))
            .collect()
    } else {
        candidates
    }
}
