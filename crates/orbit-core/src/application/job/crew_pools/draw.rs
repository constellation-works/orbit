use std::collections::BTreeMap;

use chrono::SecondsFormat;
use orbit_common::OrbitError;
use orbit_config::{CrewPerformanceMode, CrewPerformancePolicy, UsagePressurePolicy};
use orbit_store::contracts::CrewPerformanceSnapshot;
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
    /// weighting seam as task draws, with usage pressure only. Static mode consumes the same tickets
    /// over the same weights; explicit choices pass a singleton pool.
    pub(crate) fn draw_crew_pool(
        &self,
        mut candidates: Vec<CrewCandidate>,
        source: &str,
        random: &mut impl FnMut() -> Result<u64, OrbitError>,
    ) -> Result<CrewPoolDraw, OrbitError> {
        // Activity pools take no performance factor: only implementer
        // draws are scored for routing.
        let weighting = weighted_candidates(
            &mut candidates,
            &self.provider_limit_policy().usage_pressure,
            || self.usage_pressure_read(chrono::Utc::now()),
            || None,
        );
        let selected = weighted_draw(&candidates, source, random)?;
        Ok(CrewPoolDraw {
            crew: selected.crew.clone(),
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

/// The reason an inspection-requiring activity names when no crew it may
/// draw can inspect [ORB-15792].
pub(crate) const INSPECTION_CREW_UNAVAILABLE: &str = "inspection_crew_unavailable";

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

/// Tickets per unit of configured weight in a weighted draw, so a
/// weight-1 member at the `1/8` floor keeps 125 tickets [ORB-17770].
const PRESSURE_TICKET_SCALE: f64 = 1000.0;

/// One member's crew-performance factor in a task draw: the cell it was
/// read from, or why the snapshot could not be used.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PerformanceFactor {
    mode: CrewPerformanceMode,
    tier: String,
    task_type: String,
    factor: f64,
    evidence: Result<PerformanceEvidence, String>,
}

/// The cell evidence behind a usable factor.
#[derive(Debug, Clone, PartialEq)]
struct PerformanceEvidence {
    /// The cell score the advantage was read from: the cell's shrunk score,
    /// its crew mean when the crew has no cell for this type, or 100.
    score: f64,
    crew_mean: f64,
    n: u64,
    /// `cell`, `crew_mean` (no cell for the type) or `none` (no evidence
    /// for the crew's current model in this tier).
    source: &'static str,
    computed_at: String,
}

impl PerformanceFactor {
    fn json(&self) -> Value {
        let mut value = json!({
            "mode": self.mode.as_str(),
            "tier": self.tier,
            "type": self.task_type,
            "factor": self.factor,
        });
        match &self.evidence {
            Ok(evidence) => {
                value["score"] = json!(evidence.score);
                value["crew_mean"] = json!(evidence.crew_mean);
                value["n"] = json!(evidence.n);
                value["evidence"] = json!(evidence.source);
                value["computed_at"] = json!(evidence.computed_at);
            }
            Err(reason) => value["unusable"] = json!(reason),
        }
        value
    }

    /// `×1.57 (review score 115.0, crew mean 114.0)`, or `×1 (snapshot stale)`.
    fn describe(&self) -> String {
        match &self.evidence {
            Ok(evidence) => format!(
                "×{:.2} ({} score {:.1}, crew mean {:.1})",
                self.factor, self.task_type, evidence.score, evidence.crew_mean
            ),
            Err(reason) => format!("×1 (snapshot {reason})"),
        }
    }
}

/// What a task draw reads for crew performance: the policy, the task's
/// tier and type, and the workspace snapshot or why it is unusable.
pub(crate) struct CrewPerformanceRead {
    policy: CrewPerformancePolicy,
    tier: String,
    task_type: String,
    snapshot: Result<CrewPerformanceSnapshot, String>,
}

impl CrewPerformanceRead {
    pub(crate) fn new(
        policy: CrewPerformancePolicy,
        complexity: Option<TaskComplexity>,
        task_type: &str,
        snapshot: Result<CrewPerformanceSnapshot, String>,
    ) -> Self {
        Self {
            policy,
            // The scorecard files a task with no complexity under this tier.
            tier: complexity
                .map_or("unassessed", TaskComplexity::as_str)
                .to_string(),
            task_type: task_type.to_string(),
            snapshot,
        }
    }

    fn usable(&self) -> bool {
        self.snapshot.is_ok()
    }

    /// `crew`'s factor under its current model: its `(tier, type)` cell,
    /// else its crew mean for the tier, else 100. An unusable snapshot is
    /// factor 1 with the reason.
    fn member(&self, crew: &Crew) -> PerformanceFactor {
        let evidence = self
            .snapshot
            .as_ref()
            .map_err(Clone::clone)
            .map(|snapshot| {
                let tier_cells = snapshot.cells.iter().filter(|cell| {
                    cell.crew == crew.name
                        && cell.model == crew.assignment.model
                        && cell.tier == self.tier
                });
                let mut crew_mean = None;
                let mut found = None;
                for cell in tier_cells {
                    crew_mean.get_or_insert(cell.crew_mean);
                    if cell.task_type == self.task_type {
                        found = Some(cell);
                    }
                }
                let (score, crew_mean, n, source) = match (found, crew_mean) {
                    (Some(cell), _) => (cell.score, cell.crew_mean, cell.n, "cell"),
                    (None, Some(mean)) => (mean, mean, 0, "crew_mean"),
                    (None, None) => (100.0, 100.0, 0, "none"),
                };
                PerformanceEvidence {
                    score,
                    crew_mean,
                    n,
                    source,
                    computed_at: snapshot
                        .computed_at
                        .to_rfc3339_opts(SecondsFormat::Secs, true),
                }
            });
        let factor = evidence.as_ref().map_or(1.0, |evidence| {
            self.policy
                .factor(self.policy.advantage(evidence.score, evidence.crew_mean))
        });
        PerformanceFactor {
            mode: self.policy.mode,
            tier: self.tier.clone(),
            task_type: self.task_type.clone(),
            factor,
            evidence,
        }
    }
}

impl crate::OrbitRuntime {
    /// The crew-performance read of a task draw for a task of `complexity`
    /// carrying `tags` and `context_files`, or `None` when
    /// `workflow.crew_performance` is off or its strength is 0. A snapshot
    /// that is missing, stale, built from another type list or unreadable
    /// is carried as its reason, so every factor is 1 and the record says
    /// why; a draw never fails over it.
    pub(super) fn crew_performance_read(
        &self,
        complexity: Option<TaskComplexity>,
        tags: &[String],
        context_files: &[String],
    ) -> Option<CrewPerformanceRead> {
        let policy = self.provider_limit_policy().crew_performance;
        if !policy.applies() {
            return None;
        }
        let task_type =
            orbit_config::classify_task_type(self.crew_performance_types(), tags, context_files);
        let snapshot = match self.usable_crew_performance_snapshot() {
            Ok(Ok(snapshot)) => Ok(snapshot),
            Ok(Err(reason)) => Err(reason.as_str().to_string()),
            Err(error) => {
                tracing::warn!("could not read the crew-performance snapshot: {error}");
                Err("unreadable".to_string())
            }
        };
        Some(CrewPerformanceRead::new(
            policy, complexity, task_type, snapshot,
        ))
    }
}

/// One weighted member: its configured weight and the factors applied.
struct WeightedMember {
    weight: u32,
    pressure: Option<UsagePressure>,
    performance: Option<PerformanceFactor>,
}

/// What one weighting did to a draw's members, by crew name: each member's
/// configured weight, its crew's usage pressure [ORB-17770] when that
/// applies, and its crew-performance factor when that applies.
pub(crate) struct DrawWeighting {
    members: BTreeMap<String, WeightedMember>,
    /// Whether tickets were rescaled. An unusable performance snapshot with
    /// pressure off leaves the configured weights, so it draws as static.
    scaled: bool,
}

impl DrawWeighting {
    /// Whether usage pressure weighed this draw.
    pub(crate) fn pressured(&self) -> bool {
        self.members
            .values()
            .any(|member| member.pressure.is_some())
    }

    /// `crew`'s pressure in this draw.
    pub(crate) fn pressure(&self, crew: &str) -> Option<&UsagePressure> {
        self.members
            .get(crew)
            .and_then(|member| member.pressure.as_ref())
    }

    /// `crew`'s performance factor in this draw, described for history.
    pub(crate) fn performance_description(&self, crew: &str) -> Option<String> {
        self.members
            .get(crew)
            .and_then(|member| member.performance.as_ref())
            .map(PerformanceFactor::describe)
    }

    /// The mode and task type of a performance-weighted draw.
    pub(crate) fn performance_lens(&self) -> Option<(&'static str, &str)> {
        self.members
            .values()
            .find_map(|member| member.performance.as_ref())
            .map(|factor| (factor.mode.as_str(), factor.task_type.as_str()))
    }

    /// The `eligible_pool` entry of a weighted member: its configured
    /// `weight`, raw `pressure`, `performance` factor and the
    /// `effective_weight` it drew on.
    pub(crate) fn member(&self, candidate: &CrewCandidate) -> Value {
        let mut value = json!(self.pool_member(candidate));
        if let Some(performance) = self
            .members
            .get(&candidate.crew.name)
            .and_then(|member| member.performance.as_ref())
        {
            value["performance"] = performance.json();
        }
        value
    }

    fn pool_member(&self, candidate: &CrewCandidate) -> ActivityCrewPoolMember {
        let member = self.members.get(&candidate.crew.name);
        ActivityCrewPoolMember {
            name: candidate.crew.name.clone(),
            weight: member.map_or(candidate.weight, |member| member.weight),
            pressure: member
                .and_then(|member| member.pressure.as_ref())
                .map(|pressure| pressure.pressure),
            effective_weight: self
                .scaled
                .then(|| f64::from(candidate.weight) / PRESSURE_TICKET_SCALE),
        }
    }
}

/// The one weighting seam of crew pool draws: scale `candidates`' tickets
/// to `weight × pressure_factor × performance_factor`, ×1000 and rounded to
/// whole tickets. Pressure is `clamp(pressure^strength, 1/cap, cap)` from
/// the crews' weekly usage [ORB-17770]; performance is the task draw's
/// crew-performance factor, `None` for draws that take none. Apply it to a
/// pool after hold and limit narrowing; membership is unchanged. Task
/// admission applies its allowlist afterward; other draws keep their
/// existing membership filters first. A weight of 0 stays 0 and a positive
/// weight keeps at least one ticket. `None` leaves the draw as it was:
/// neither factor applies or there is no choice to weigh.
pub(crate) fn weighted_candidates(
    candidates: &mut [CrewCandidate],
    pressure_policy: &UsagePressurePolicy,
    pressure_read: impl FnOnce() -> UsagePressureRead,
    performance_read: impl FnOnce() -> Option<CrewPerformanceRead>,
) -> Option<DrawWeighting> {
    if candidates.len() < 2 {
        return None;
    }
    let pressure = pressure_policy.applies().then(pressure_read);
    let performance = performance_read();
    if pressure.is_none() && performance.is_none() {
        return None;
    }
    let scaled = pressure.is_some()
        || performance
            .as_ref()
            .is_some_and(CrewPerformanceRead::usable);
    let mut members = BTreeMap::new();
    for candidate in candidates.iter_mut() {
        let crew_pressure = pressure.as_ref().map(|read| read.crew(&candidate.crew));
        let crew_performance = performance
            .as_ref()
            .map(|read| read.member(&candidate.crew));
        let configured = candidate.weight;
        if configured > 0 && scaled {
            let multiplier = crew_pressure.as_ref().map_or(1.0, |pressure| {
                pressure_policy.multiplier(pressure.pressure)
            }) * crew_performance
                .as_ref()
                .map_or(1.0, |performance| performance.factor);
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
                performance: crew_performance,
            },
        );
    }
    Some(DrawWeighting { members, scaled })
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
