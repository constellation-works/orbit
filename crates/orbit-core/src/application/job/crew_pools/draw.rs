use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_config::UsagePressurePolicy;
use orbit_types::identity::Crew;
use orbit_types::task::TaskComplexity;
use orbit_types::workflow::ActivityCrewPoolMember;
use orbit_types::workflow::activity_job::Provider;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::application::contracts::{CrewCandidate, CrewPoolDraw};
use crate::application::task::UsagePressure;
use crate::application::task::provider_limit::UsagePressureRead;
use crate::runtime::engine::crew::{CrewAllowlist, enforce_crew_allowlist};
use crate::runtime::run_input::singular_task_id_from_input;

use super::types::{COMPLEXITIES, CapturedCrewPools, POOLS_KEY};

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

impl crate::OrbitRuntime {
    /// Draw after the caller's existing membership filters, using the same
    /// weighting seam as task draws, with usage pressure only. Static mode consumes the same tickets
    /// over the same weights; explicit choices pass a singleton pool.
    pub(crate) fn draw_crew_pool(
        &self,
        mut candidates: Vec<CrewCandidate>,
        source: &str,
        random: &mut impl FnMut() -> Result<u64, OrbitError>,
    ) -> Result<CrewPoolDraw, OrbitError> {
        let weighting = weighted_candidates(
            &mut candidates,
            &self.provider_limit_policy().usage_pressure,
            || self.usage_pressure_read(chrono::Utc::now()),
        );
        let selected = weighted_draw(&candidates, source, random)?;
        Ok(CrewPoolDraw {
            crew: selected.crew.clone(),
            auto_effort: selected.auto_effort.clone(),
            source: if weighting.as_ref().is_some_and(DrawWeighting::pressured) {
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

impl crate::OrbitRuntime {
    /// Whether `crew` can run a pinned-source inspection activity: its
    /// provider has a CLI runtime and its executor declares
    /// `inspection_args`, the arguments that turn on its native file-read
    /// and shell tools. Nothing here names a provider; the declaration lives
    /// in executor configuration. A crew that cannot is passed over before
    /// dispatch rather than launched to fail its observed-revision proof.
    pub(crate) fn can_inspect(&self, crew: &Crew) -> Result<bool, OrbitError> {
        let provider = crew.assignment.provider.as_str();
        if !Provider::parse(provider).is_ok_and(Provider::has_cli_runtime) {
            return Ok(false);
        }
        let installed = self.get_executor_def(provider)?;
        Ok(
            crate::application::executor::declared_inspection_args(installed.as_ref(), provider)
                .is_some(),
        )
    }

    /// `candidates` split into the crews that can inspect and the rest.
    pub(super) fn partition_inspecting(
        &self,
        candidates: Vec<CrewCandidate>,
    ) -> Result<(Vec<CrewCandidate>, Vec<CrewCandidate>), OrbitError> {
        let mut capable = Vec::new();
        let mut unable = Vec::new();
        for candidate in candidates {
            if self.can_inspect(&candidate.crew)? {
                capable.push(candidate);
            } else {
                unable.push(candidate);
            }
        }
        Ok((capable, unable))
    }

    /// Whether `crew` can read images it produces itself mid-run, as a
    /// `verify-mock-ui` implementer's self-check requires (visual-review
    /// §5.3): its provider has a CLI runtime, its executor declares
    /// `image_input` with mode `native_read` (its own file-read tool
    /// presents image files to the model), and the crew does not set
    /// `vision = false`. An `attach_args` lane receives images only at
    /// launch, so it cannot read self-check output and counts as without
    /// vision for implementation (§9.1).
    pub(crate) fn can_read_images(&self, crew: &Crew) -> Result<bool, OrbitError> {
        if !crew.vision {
            return Ok(false);
        }
        let provider = crew.assignment.provider.as_str();
        if !Provider::parse(provider).is_ok_and(Provider::has_cli_runtime) {
            return Ok(false);
        }
        let installed = self.get_executor_def(provider)?;
        Ok(
            crate::application::executor::declared_image_input(installed.as_ref(), provider)
                .is_some_and(|declared| {
                    declared.mode == orbit_types::workflow::ImageInputMode::NativeRead
                }),
        )
    }

    /// Whether `crew` can be given images at launch, as a visual
    /// reviewer or verifier is (visual-review §9.4): its provider has a
    /// CLI runtime, its executor declares `image_input` in either
    /// mode, and the crew does not set `vision = false`. Unlike
    /// [`Self::can_read_images`], an `attach_args` lane qualifies: the
    /// images are attached when the invocation starts.
    pub(crate) fn can_take_images(
        &self,
        crew: &Crew,
    ) -> Result<Option<orbit_types::workflow::ExecutorImageInput>, OrbitError> {
        if !crew.vision {
            return Ok(None);
        }
        let provider = crew.assignment.provider.as_str();
        if !Provider::parse(provider).is_ok_and(Provider::has_cli_runtime) {
            return Ok(None);
        }
        let installed = self.get_executor_def(provider)?;
        Ok(crate::application::executor::declared_image_input(
            installed.as_ref(),
            provider,
        ))
    }

    /// `candidates` split into the crews that can read images and the rest.
    pub(super) fn partition_image_reading(
        &self,
        candidates: Vec<CrewCandidate>,
    ) -> Result<(Vec<CrewCandidate>, Vec<CrewCandidate>), OrbitError> {
        let mut capable = Vec::new();
        let mut unable = Vec::new();
        for candidate in candidates {
            if self.can_read_images(&candidate.crew)? {
                capable.push(candidate);
            } else {
                unable.push(candidate);
            }
        }
        Ok((capable, unable))
    }

    /// The draw candidates of a `verify-mock-ui` implementer (§5.3): the
    /// image-capable members of `candidates` while the pool has one, with
    /// `source` naming the filter; otherwise the unfiltered pool with
    /// `source` carrying [`IMPLEMENTER_WITHOUT_VISION`], which the caller
    /// also records on the run. A task without the tag draws unchanged.
    pub(super) fn image_capable_implementer_candidates(
        &self,
        tags: &[String],
        candidates: Vec<CrewCandidate>,
        source: String,
    ) -> Result<(Vec<CrewCandidate>, String, bool), OrbitError> {
        if !orbit_types::task::has_verify_mock_ui_tag(tags) {
            return Ok((candidates, source, false));
        }
        let (capable, unable) = self.partition_image_reading(candidates.clone())?;
        if capable.is_empty() {
            return Ok((
                candidates,
                format!("{source}; {IMPLEMENTER_WITHOUT_VISION}"),
                true,
            ));
        }
        if unable.is_empty() {
            return Ok((capable, source, false));
        }
        let skipped = unable
            .iter()
            .map(|candidate| candidate.crew.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        Ok((
            capable,
            format!("{source}; image-capable implementer (verify-mock-ui), skipped: {skipped}"),
            false,
        ))
    }
}

/// Recorded on a `verify-mock-ui` run whose implementer was drawn from a
/// pool with no image-capable crew (visual-review §5.3).
pub(crate) const IMPLEMENTER_WITHOUT_VISION: &str = "implementer_without_vision";

pub(super) fn sole_candidate(crew: Crew) -> CrewCandidate {
    CrewCandidate {
        crew,
        weight: 1,
        auto_effort: None,
    }
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

/// Tickets per unit of configured weight in a weighted draw, so a
/// weight-1 member at the `1/8` floor keeps 125 tickets [ORB-17770].
const PRESSURE_TICKET_SCALE: f64 = 1000.0;

/// One weighted member: its configured weight and its crew's pressure.
struct WeightedMember {
    weight: u32,
    pressure: UsagePressure,
}

/// What one weighting did to a draw's members, by crew name: each member's
/// configured weight and its crew's usage pressure [ORB-17770].
pub(crate) struct DrawWeighting {
    members: BTreeMap<String, WeightedMember>,
}

impl DrawWeighting {
    /// Whether usage pressure weighed this draw: always, as a weighting
    /// exists only when pressure applies.
    pub(crate) fn pressured(&self) -> bool {
        !self.members.is_empty()
    }

    /// `crew`'s pressure in this draw.
    pub(crate) fn pressure(&self, crew: &str) -> Option<&UsagePressure> {
        self.members.get(crew).map(|member| &member.pressure)
    }

    /// The `eligible_pool` entry of a weighted member: its configured
    /// `weight`, raw `pressure` and the `effective_weight` it drew on.
    pub(crate) fn member(&self, candidate: &CrewCandidate) -> Value {
        json!(self.pool_member(candidate))
    }

    fn pool_member(&self, candidate: &CrewCandidate) -> ActivityCrewPoolMember {
        let member = self.members.get(&candidate.crew.name);
        ActivityCrewPoolMember {
            name: candidate.crew.name.clone(),
            weight: member.map_or(candidate.weight, |member| member.weight),
            pressure: member.map(|member| member.pressure.pressure),
            effective_weight: Some(f64::from(candidate.weight) / PRESSURE_TICKET_SCALE),
        }
    }
}

/// The one weighting seam of crew pool draws: scale `candidates`' tickets
/// to `weight × pressure_factor`, ×1000 and rounded to whole tickets, where
/// the factor is `clamp(pressure^strength, 1/cap, cap)` from the crews'
/// weekly usage [ORB-17770]. Apply it to a pool after hold and limit
/// narrowing; membership is unchanged. Task admission applies its allowlist
/// afterward; other draws keep their existing membership filters first. A
/// weight of 0 stays 0 and a positive weight keeps at least one ticket.
/// `None` leaves the draw as it was: pressure does not apply or there is no
/// choice to weigh.
pub(crate) fn weighted_candidates(
    candidates: &mut [CrewCandidate],
    pressure_policy: &UsagePressurePolicy,
    pressure_read: impl FnOnce() -> UsagePressureRead,
) -> Option<DrawWeighting> {
    if candidates.len() < 2 || !pressure_policy.applies() {
        return None;
    }
    let pressure = pressure_read();
    let mut members = BTreeMap::new();
    for candidate in candidates.iter_mut() {
        let crew_pressure = pressure.crew(&candidate.crew);
        let configured = candidate.weight;
        if configured > 0 {
            let multiplier = pressure_policy.multiplier(crew_pressure.pressure);
            let tickets = (f64::from(configured) * multiplier * PRESSURE_TICKET_SCALE)
                .round()
                .clamp(1.0, f64::from(u32::MAX));
            // In range by the clamp above.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                candidate.weight = tickets as u32;
            }
        }
        members.insert(
            candidate.crew.name.clone(),
            WeightedMember {
                weight: configured,
                pressure: crew_pressure,
            },
        );
    }
    Some(DrawWeighting { members })
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
