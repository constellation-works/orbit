use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_config::UsagePressurePolicy;
use orbit_types::identity::Crew;
use orbit_types::task::TaskComplexity;
use orbit_types::workflow::activity_job::Provider;
use orbit_types::workflow::{ActivityCrewDraw, ActivityCrewPoolMember};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::application::task::UsagePressure;
use crate::application::task::provider_limit::UsagePressureRead;
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

impl crate::OrbitRuntime {
    /// Draw after the caller's existing membership filters, using the same
    /// pressure helper as task draws. Static mode consumes the same tickets
    /// over the same weights; explicit choices pass a singleton pool.
    pub(crate) fn draw_crew_pool(
        &self,
        mut candidates: Vec<CrewCandidate>,
        source: &str,
        random: &mut impl FnMut() -> Result<u64, OrbitError>,
    ) -> Result<CrewPoolDraw, OrbitError> {
        let weighting = pressure_weighted_candidates(
            &mut candidates,
            &self.provider_limit_policy().usage_pressure,
            || self.usage_pressure_read(chrono::Utc::now()),
        );
        let selected = weighted_draw(&candidates, source, random)?;
        Ok(CrewPoolDraw {
            crew: selected.crew.clone(),
            source: if weighting.is_some() {
                format!("{source}; usage pressure")
            } else {
                source.to_string()
            },
            eligible_pool: candidates
                .iter()
                .map(|candidate| {
                    weighting.as_ref().map_or_else(
                        || ActivityCrewPoolMember {
                            name: candidate.crew.name.clone(),
                            weight: candidate.weight,
                            pressure: None,
                            effective_weight: None,
                        },
                        |weighting| weighting.pool_member(candidate),
                    )
                })
                .collect(),
        })
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

/// Tickets per unit of configured weight in a usage-pressure draw, so a
/// weight-1 member at the `1/8` floor keeps 125 tickets [ORB-17770].
const PRESSURE_TICKET_SCALE: f64 = 1000.0;

/// What one usage-pressure weighting did to a draw's members [ORB-17770]:
/// each member's configured weight and crew pressure, by crew name.
pub(crate) struct PressureWeighting {
    members: BTreeMap<String, (u32, UsagePressure)>,
}

impl PressureWeighting {
    /// `crew`'s pressure in this draw.
    pub(crate) fn pressure(&self, crew: &str) -> Option<&UsagePressure> {
        self.members.get(crew).map(|(_, pressure)| pressure)
    }

    /// The `eligible_pool` entry of a weighted member: its configured
    /// `weight`, raw `pressure` and the `effective_weight` it drew on.
    pub(crate) fn member(&self, candidate: &CrewCandidate) -> Value {
        json!(self.pool_member(candidate))
    }

    fn pool_member(&self, candidate: &CrewCandidate) -> ActivityCrewPoolMember {
        let (weight, pressure) = self
            .members
            .get(&candidate.crew.name)
            .map_or((candidate.weight, 1.0), |(weight, pressure)| {
                (*weight, pressure.pressure)
            });
        ActivityCrewPoolMember {
            name: candidate.crew.name.clone(),
            weight,
            pressure: Some(pressure),
            effective_weight: Some(f64::from(candidate.weight) / PRESSURE_TICKET_SCALE),
        }
    }
}

/// Scale `candidates`' tickets by their crews' weekly usage pressure:
/// `weight × clamp(pressure^strength, 1/cap, cap)`, ×1000 and rounded to
/// whole tickets [ORB-17770]. Apply it to a pool after hold and limit
/// narrowing; membership is unchanged. Task admission applies its allowlist
/// afterward; other draws keep their existing membership filters first.
/// A weight of 0 stays 0 and a positive weight keeps at least one ticket.
/// `None` leaves the draw as it was: the policy is off or there is no
/// choice to weigh.
pub(crate) fn pressure_weighted_candidates(
    candidates: &mut [CrewCandidate],
    policy: &UsagePressurePolicy,
    read: impl FnOnce() -> UsagePressureRead,
) -> Option<PressureWeighting> {
    if !policy.applies() || candidates.len() < 2 {
        return None;
    }
    let read = read();
    let mut members = BTreeMap::new();
    for candidate in candidates.iter_mut() {
        let pressure = read.crew(&candidate.crew);
        let configured = candidate.weight;
        if configured > 0 {
            let tickets = (f64::from(configured)
                * policy.multiplier(pressure.pressure)
                * PRESSURE_TICKET_SCALE)
                .round()
                .clamp(1.0, f64::from(u32::MAX));
            // In range by the clamp above.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                candidate.weight = tickets as u32;
            }
        }
        members.insert(candidate.crew.name.clone(), (configured, pressure));
    }
    Some(PressureWeighting { members })
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
