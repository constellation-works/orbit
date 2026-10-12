use orbit_common::OrbitError;
use orbit_types::identity::Crew;
use orbit_types::task::TaskComplexity;
use orbit_types::workflow::activity_job::Provider;
use orbit_types::workflow::{ActivityCrewDraw, ActivityCrewPoolMember};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::runtime::engine::crew::{CrewAllowlist, enforce_crew_allowlist};
use crate::runtime::run_input::singular_task_id_from_input;

use super::types::{COMPLEXITIES, CapturedCrewPools, CrewCandidate, POOLS_KEY};

pub(super) fn pools_from_input(input: &Value) -> Result<CapturedCrewPools, OrbitError> {
    input
        .get(POOLS_KEY)
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|error| OrbitError::InvalidInput(format!("invalid {POOLS_KEY}: {error}")))
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

pub(super) fn auto_task_id(input: &Value) -> Option<&str> {
    singular_task_id_from_input(input)
}

/// The configuration key of the complexity pool for `complexity`, as an
/// activity's `crew_config_key` names it [ORB-15663].
pub(crate) fn complexity_crews_key(complexity: TaskComplexity) -> String {
    format!("workflow.{complexity}_complexity_crews")
}

/// The complexity whose pool `key` names, or `None` for any other key.
pub(crate) fn complexity_of_crews_key(key: &str) -> Option<TaskComplexity> {
    COMPLEXITIES
        .into_iter()
        .find(|complexity| complexity_crews_key(*complexity) == key)
}

/// The run-state record of an activity crew draw over `candidates`.
pub(super) fn activity_crew_draw(
    selected: &CrewCandidate,
    source: &str,
    candidates: &[CrewCandidate],
) -> ActivityCrewDraw {
    ActivityCrewDraw {
        crew: selected.crew.name.clone(),
        source: source.to_string(),
        eligible_pool: candidates
            .iter()
            .map(|candidate| ActivityCrewPoolMember {
                name: candidate.crew.name.clone(),
                weight: candidate.weight,
            })
            .collect(),
    }
}

/// The reason an inspection-requiring activity names when no crew it may
/// draw can inspect [ORB-15792].
pub(crate) const INSPECTION_CREW_UNAVAILABLE: &str = "inspection_crew_unavailable";

/// Whether `crew`'s provider has a native file-read and command surface.
pub(crate) fn can_inspect(crew: &Crew) -> bool {
    Provider::parse(&crew.assignment.provider).is_ok_and(Provider::has_native_inspection)
}

pub(super) fn sole_candidate(crew: Crew) -> CrewCandidate {
    CrewCandidate { crew, weight: 1 }
}

/// The members this run may actually draw, renormalised over the allowlist.
///
/// A parked crew (weight `0`) holds no ticket, so it neither wins a draw nor
/// rescues a pool the allowlist has otherwise emptied.
pub(super) fn permitted_candidates(
    candidates: Vec<CrewCandidate>,
    source: &str,
    allowlist: Option<&CrewAllowlist>,
) -> Result<Vec<CrewCandidate>, OrbitError> {
    if let [only] = candidates.as_slice()
        && only.weight > 0
    {
        enforce_crew_allowlist(allowlist, &only.crew, source)?;
        return Ok(candidates);
    }
    let has_positive_weight = candidates.iter().any(|candidate| candidate.weight > 0);
    let names = candidates
        .iter()
        .map(|candidate| candidate.crew.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let permitted = candidates
        .into_iter()
        .filter(|candidate| {
            candidate.weight > 0 && allowlist.is_none_or(|list| list.permits(&candidate.crew))
        })
        .collect::<Vec<_>>();
    if permitted.is_empty() {
        let reason = if has_positive_weight {
            "has no member permitted by this run's crew allowlist"
        } else {
            "has no member with a weight above 0"
        };
        return Err(OrbitError::InvalidInput(format!(
            "crew pool from {source} [{names}] {reason}"
        )));
    }
    Ok(permitted)
}

/// Draw one ticket in `[0, total_weight)` and walk the cumulative weights.
/// An all-bare pool weighs one ticket per member, so it draws exactly as the
/// uniform selector it replaces did, on the same ticket.
pub(crate) fn weighted_draw<'a>(
    candidates: &'a [CrewCandidate],
    source: &str,
    random: &mut impl FnMut() -> Result<u64, OrbitError>,
) -> Result<&'a CrewCandidate, OrbitError> {
    if let [only] = candidates {
        return if only.weight > 0 {
            Ok(only)
        } else {
            Err(OrbitError::InvalidInput(format!(
                "crew pool from {source} has no member with a weight above 0"
            )))
        };
    }
    let total = candidates
        .iter()
        .map(|candidate| u64::from(candidate.weight))
        .sum();
    let ticket = random_ticket(total, source, random)?;
    let mut cumulative = 0;
    for candidate in candidates {
        cumulative += u64::from(candidate.weight);
        if ticket < cumulative {
            return Ok(candidate);
        }
    }
    // `permitted_candidates` keeps only positive weights, so the walk always
    // lands inside the pool; report the impossible rather than fall through to
    // an arbitrary member.
    Err(OrbitError::Execution(format!(
        "crew pool from {source} drew ticket {ticket} outside its {total} weighted tickets"
    )))
}

fn random_ticket(
    bound: u64,
    source: &str,
    random: &mut impl FnMut() -> Result<u64, OrbitError>,
) -> Result<u64, OrbitError> {
    if bound == 0 {
        return Err(OrbitError::InvalidInput(format!(
            "crew pool from {source} has no member with a weight above 0"
        )));
    }
    // Rejection sampling avoids modulo bias for totals that are not a power of
    // two. No random draw occurs for explicit or singleton choices.
    let threshold = bound.wrapping_neg() % bound;
    loop {
        let ticket = random()?;
        if ticket >= threshold {
            return Ok(ticket % bound);
        }
    }
}

pub(crate) fn random_crew_ticket() -> Result<u64, OrbitError> {
    getrandom::u64().map_err(|error| OrbitError::Execution(format!("draw automatic crew: {error}")))
}

/// Reproducible tickets for a draw that must come out the same when it is
/// retried: each is the leading eight bytes of `sha256("<seed>:<round>")`.
pub(crate) fn seeded_crew_ticket(seed: String) -> impl FnMut() -> Result<u64, OrbitError> {
    let mut round: u64 = 0;
    move || {
        let digest = Sha256::digest(format!("{seed}:{round}").as_bytes());
        round += 1;
        let mut ticket = [0u8; 8];
        ticket.copy_from_slice(&digest[..8]);
        Ok(u64::from_be_bytes(ticket))
    }
}
