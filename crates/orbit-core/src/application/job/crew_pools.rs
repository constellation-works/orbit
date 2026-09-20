//! Capture pool policy on the admitting pipeline and freeze each task's
//! selection in its run input. Descendant pipelines inherit the same task
//! choice.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_config::{
    ComplexityCrewPools, CrewPoolEntry, canonical_crew_pool, canonical_crew_pool_entries,
};
use orbit_types::identity::Crew;
use orbit_types::task::{Task, TaskComplexity};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::runtime::engine::crew::{CrewAllowlist, enforce_crew_allowlist};
use crate::runtime::run_input::{non_empty, singular_task_id_from_input};

const POOLS_KEY: &str = "auto_crew_pools";
/// The pipelines complexity-pool routing applies to: the two workspace
/// coordinators and the task-carrying delivery pipelines [ORB-12606].
///
/// Policy is captured by whichever of these is admitted without a
/// policy-bearing parent, so an ordinary `run ship` routes a crew-less task
/// exactly as a drain does. System, review and preparation jobs are absent
/// from the list and keep their own crew selection.
const POLICY_PIPELINES: [&str; 6] = [
    "workspace_auto_pipeline",
    "workspace_ship_pipeline",
    "task_auto_pipeline",
    "task_gate_pipeline",
    "task_local_pipeline",
    "task_pr_pipeline",
];
const SELECTION_KEY: &str = "crew_selection";
const COMPLEXITIES: [TaskComplexity; 4] = [
    TaskComplexity::Low,
    TaskComplexity::Medium,
    TaskComplexity::Hard,
    TaskComplexity::XHard,
];

/// Drawn crew to persist onto a crew-less task at the in-progress transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DispatchedCrewStamp {
    pub(crate) crew: String,
    pub(crate) source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CapturedCrewPool {
    /// Weighted members. A pool captured before weights existed is a plain
    /// name list, which [`CrewPoolEntry`] still deserialises at weight 1.
    crews: Vec<CrewPoolEntry>,
    source: String,
}

/// One crew a task may be admitted to, with the tickets it holds in the draw.
/// A crew chosen outside a pool — explicit, `task.crew`, or the default chain —
/// is the only candidate and holds a single ticket.
#[derive(Debug, Clone)]
pub(crate) struct CrewCandidate {
    pub(crate) crew: Crew,
    pub(crate) weight: u32,
}

pub(crate) type CapturedCrewPools = BTreeMap<String, CapturedCrewPool>;

impl OrbitRuntime {
    /// Transport inputs only; validation and source capture happen at the
    /// common pipeline admission boundary, including generic job submissions.
    pub(crate) fn set_auto_crew_overrides(input: &mut Value, overrides: &ComplexityCrewPools) {
        for complexity in COMPLEXITIES {
            if let Some(pool) = overrides.pool(complexity) {
                input[format!("{complexity}_complexity_crews")] = json!(pool);
            }
        }
    }

    fn capture_auto_crew_pools(&self, input: &Value) -> Result<CapturedCrewPools, OrbitError> {
        COMPLEXITIES
            .into_iter()
            .map(|complexity| {
                let key = format!("{complexity}_complexity_crews");
                let (names, source) = match input.get(&key) {
                    Some(raw) => (
                        serde_json::from_value::<Vec<String>>(raw.clone()).map_err(|error| {
                            OrbitError::InvalidInput(format!(
                                "{key} must be an array of crew names: {error}"
                            ))
                        })?,
                        format!("run_input.{key}"),
                    ),
                    None => (
                        self.context
                            .settings()
                            .complexity_crews()
                            .pool(complexity)
                            .unwrap_or_default()
                            .to_vec(),
                        format!("workflow.{key}"),
                    ),
                };
                let pool = canonical_crew_pool(&names, self.context.settings().crews(), &source)?;
                Ok((
                    complexity.to_string(),
                    CapturedCrewPool {
                        crews: pool.entries,
                        source,
                    },
                ))
            })
            .collect()
    }

    /// Classifiers use the coordinator's frozen policy, including CLI overrides.
    /// A read-only call without a run uses the current configuration.
    pub(crate) fn auto_crew_pools_for_input(
        &self,
        input: &Value,
    ) -> Result<CapturedCrewPools, OrbitError> {
        if let Some(run_id) = input
            .get("run_id")
            .and_then(Value::as_str)
            .and_then(non_empty)
            && let Some(run) = self.get_job_run_backend(run_id)?
        {
            return pools_from_input(run.input.as_ref().unwrap_or(&Value::Null));
        }
        if input.get(POOLS_KEY).is_some() {
            pools_from_input(input)
        } else {
            self.capture_auto_crew_pools(input)
        }
    }

    /// One selection seam for both read-only eligibility and admission. The
    /// former inspects the candidates without drawing a random ticket.
    pub(crate) fn auto_task_crew_candidates(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
        explicit: Option<&str>,
    ) -> Result<(Vec<CrewCandidate>, String), OrbitError> {
        if explicit.and_then(non_empty).is_some()
            || task.crew.as_deref().and_then(non_empty).is_some()
        {
            return Ok((
                vec![sole_candidate(
                    self.resolve_crew_for_task(explicit, task.crew.as_deref())?,
                )],
                if explicit.and_then(non_empty).is_some() {
                    "explicit"
                } else {
                    "task.crew"
                }
                .to_string(),
            ));
        }
        if let Some(complexity) = task.complexity
            && let Some(pool) = pools.get(complexity.as_str())
            && !pool.crews.is_empty()
        {
            let entries = canonical_crew_pool_entries(
                &pool.crews,
                self.context.settings().crews(),
                &pool.source,
            )?;
            let crews = entries
                .iter()
                .map(|entry| {
                    Ok(CrewCandidate {
                        crew: self.resolve_crew_for_task(Some(&entry.name), None)?,
                        weight: entry.weight,
                    })
                })
                .collect::<Result<Vec<_>, OrbitError>>()?;
            return Ok((crews, pool.source.clone()));
        }
        Ok((
            vec![sole_candidate(self.effective_task_crew(task)?)],
            "default".to_string(),
        ))
    }

    /// Called before the existing durable insert. Resume input already contains
    /// the chosen crew; neither resume nor same-task child admission rerolls it.
    /// Randomness is injectable at this application boundary for deterministic
    /// tests of admission, inheritance and rejection sampling.
    pub(crate) fn install_auto_crew_admission(
        &self,
        job_name: &str,
        input: &mut Value,
        parent_run_id: Option<&str>,
        resuming: bool,
        random: &mut impl FnMut() -> Result<u64, OrbitError>,
    ) -> Result<(), OrbitError> {
        if resuming {
            return Ok(());
        }
        if !POLICY_PIPELINES.contains(&job_name) {
            return Ok(());
        }
        if !input.is_object() {
            return Err(OrbitError::InvalidInput(
                "pipeline run input must be a JSON object".to_string(),
            ));
        }
        let parent_input = parent_run_id
            .map(|run_id| self.get_job_run_backend(run_id))
            .transpose()?
            .flatten()
            .and_then(|run| run.input)
            .filter(|parent| parent.get(POOLS_KEY).is_some());
        match parent_input {
            // A descendant runs under the policy its coordinator froze,
            // including that run's overrides and crew allowlist.
            Some(parent) => {
                input[POOLS_KEY] = parent[POOLS_KEY].clone();
                // Keep separately explicit constraints authoritative through every child.
                if let Some(allowed) = parent.get("allowed_crews") {
                    input["allowed_crews"] = allowed.clone();
                }
                let Some(task_id) = auto_task_id(input).map(ToOwned::to_owned) else {
                    return Ok(());
                };
                if let Some(selection) = parent.get(SELECTION_KEY)
                    && selection.get("task_id").and_then(Value::as_str) == Some(&task_id)
                {
                    input["crew"] = selection["crew"].clone();
                    input[SELECTION_KEY] = selection.clone();
                    return Ok(());
                }
                self.capture_auto_task_selection(input, &task_id, random)
            }
            // A top-level submission — a drain, a ship wrapper, or an ordinary
            // `run ship` — captures the effective policy itself. A submission
            // naming exactly one task then draws that task's crew here; a
            // multi-task or discovery run leaves each leaf to draw at its own
            // admission, so siblings stay independent.
            None => {
                input[POOLS_KEY] = json!(self.capture_auto_crew_pools(input)?);
                let Some(task_id) = auto_task_id(input).map(ToOwned::to_owned) else {
                    return Ok(());
                };
                self.capture_auto_task_selection(input, &task_id, random)
            }
        }
    }

    /// Report a submission-time crew exclusion against the crew that will
    /// actually run [ORB-12606]. A task routed by a pool is excluded only when
    /// the allowlist permits none of the pool's members, which is exactly what
    /// admission will decide; a task with one candidate is named directly.
    pub(crate) fn enforce_admitted_crew_allowlist(
        &self,
        task: &Task,
        input: &Value,
        allowlist: &CrewAllowlist,
        origin: &str,
    ) -> Result<(), OrbitError> {
        let pools = self.auto_crew_pools_for_input(input)?;
        let explicit = input
            .get("crew")
            .and_then(Value::as_str)
            .and_then(non_empty);
        let (candidates, source) = self.auto_task_crew_candidates(task, &pools, explicit)?;
        if let [only] = candidates.as_slice() {
            return enforce_crew_allowlist(Some(allowlist), &only.crew, origin);
        }
        permitted_candidates(candidates, &source, Some(allowlist)).map(|_| ())
    }

    fn capture_auto_task_selection(
        &self,
        input: &mut Value,
        task_id: &str,
        random: &mut impl FnMut() -> Result<u64, OrbitError>,
    ) -> Result<(), OrbitError> {
        let pools = pools_from_input(input)?;
        let task = self.get_task(task_id)?;
        let explicit = input
            .get("crew")
            .and_then(Value::as_str)
            .and_then(non_empty);
        let (crews, source) = self.auto_task_crew_candidates(&task, &pools, explicit)?;
        let allowlist = self.crew_allowlist_from_input(input)?;
        let candidates = permitted_candidates(crews, &source, allowlist.as_ref())?;
        let selected = weighted_draw(&candidates, &source, random)?;
        input[SELECTION_KEY] = json!({
            "task_id": task.id,
            "crew": selected.crew.name,
            "source": source,
            "complexity": task.complexity,
            // The odds this draw ran on, renormalised over the permitted
            // members, so `orbit run show` can explain the choice.
            "eligible_pool": candidates
                .iter()
                .map(|candidate| json!({"name": candidate.crew.name, "weight": candidate.weight}))
                .collect::<Vec<_>>(),
        });
        input["crew"] = json!(selected.crew.name);
        Ok(())
    }

    /// The crew `install_auto_crew_admission` froze for `task_id`, if this run
    /// went through the pool seam. Missing runs, system jobs, and selections
    /// for a different task yield `None`.
    pub(crate) fn dispatched_crew_stamp(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<Option<DispatchedCrewStamp>, OrbitError> {
        let Some(run) = self.get_job_run_backend(run_id)? else {
            return Ok(None);
        };
        Ok(dispatched_crew_stamp_from_input(
            run.input.as_ref().unwrap_or(&Value::Null),
            task_id,
        ))
    }
}

fn pools_from_input(input: &Value) -> Result<CapturedCrewPools, OrbitError> {
    input
        .get(POOLS_KEY)
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|error| OrbitError::InvalidInput(format!("invalid {POOLS_KEY}: {error}")))
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

fn auto_task_id(input: &Value) -> Option<&str> {
    singular_task_id_from_input(input)
}

fn dispatched_crew_stamp_from_input(input: &Value, task_id: &str) -> Option<DispatchedCrewStamp> {
    let selection = input.get(SELECTION_KEY)?;
    if selection.get("task_id").and_then(Value::as_str) != Some(task_id) {
        return None;
    }
    let crew = selection
        .get("crew")
        .and_then(Value::as_str)
        .and_then(non_empty)?
        .to_string();
    Some(DispatchedCrewStamp {
        crew,
        source: crew_stamp_source(selection),
    })
}

/// History provenance for a stamped crew: `explicit`, `task.crew`, `default`,
/// or `pool:<complexity>`. Pool sources are stored on the run as
/// `workflow.*_complexity_crews` / `run_input.*_complexity_crews`.
fn crew_stamp_source(selection: &Value) -> String {
    let raw = selection
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("default");
    match raw {
        "explicit" | "task.crew" | "default" => raw.to_string(),
        other => {
            if let Some(complexity) = selection
                .get("complexity")
                .and_then(Value::as_str)
                .and_then(non_empty)
            {
                format!("pool:{complexity}")
            } else {
                COMPLEXITIES
                    .into_iter()
                    .find(|complexity| other.contains(&format!("{complexity}_complexity_crews")))
                    .map(|complexity| format!("pool:{complexity}"))
                    .unwrap_or_else(|| other.to_string())
            }
        }
    }
}

fn sole_candidate(crew: Crew) -> CrewCandidate {
    CrewCandidate { crew, weight: 1 }
}

/// The members this run may actually draw, renormalised over the allowlist.
///
/// A parked crew (weight `0`) holds no ticket, so it neither wins a draw nor
/// rescues a pool the allowlist has otherwise emptied.
fn permitted_candidates(
    candidates: Vec<CrewCandidate>,
    source: &str,
    allowlist: Option<&CrewAllowlist>,
) -> Result<Vec<CrewCandidate>, OrbitError> {
    if candidates.len() == 1 {
        enforce_crew_allowlist(allowlist, &candidates[0].crew, source)?;
        return Ok(candidates);
    }
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
        return Err(OrbitError::InvalidInput(format!(
            "crew pool from {source} [{names}] has no member permitted by this run's crew allowlist"
        )));
    }
    Ok(permitted)
}

/// Draw one ticket in `[0, total_weight)` and walk the cumulative weights.
/// An all-bare pool weighs one ticket per member, so it draws exactly as the
/// uniform selector it replaces did, on the same ticket.
fn weighted_draw<'a>(
    candidates: &'a [CrewCandidate],
    source: &str,
    random: &mut impl FnMut() -> Result<u64, OrbitError>,
) -> Result<&'a CrewCandidate, OrbitError> {
    if let [only] = candidates {
        return Ok(only);
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
