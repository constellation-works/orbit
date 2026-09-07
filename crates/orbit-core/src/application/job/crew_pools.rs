//! Capture auto policy on the coordinator and freeze each task's selection in
//! its admitted run input. Descendant pipelines inherit the same task choice.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_config::{ComplexityCrewPools, canonical_crew_pool};
use orbit_types::identity::Crew;
use orbit_types::task::{Task, TaskComplexity};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::runtime::engine::crew::{CrewAllowlist, enforce_crew_allowlist};
use crate::runtime::run_input::{non_empty, singular_task_id_from_input};

const POOLS_KEY: &str = "auto_crew_pools";
const SELECTION_KEY: &str = "crew_selection";
const COMPLEXITIES: [TaskComplexity; 3] = [
    TaskComplexity::Low,
    TaskComplexity::Medium,
    TaskComplexity::Hard,
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CapturedCrewPool {
    crews: Vec<String>,
    source: String,
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
                let crews = canonical_crew_pool(&names, self.context.settings().crews(), &source)?;
                Ok((complexity.to_string(), CapturedCrewPool { crews, source }))
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
    ) -> Result<(Vec<Crew>, String), OrbitError> {
        if explicit.and_then(non_empty).is_some()
            || task.crew.as_deref().and_then(non_empty).is_some()
        {
            return Ok((
                vec![self.resolve_crew_for_task(explicit, task.crew.as_deref())?],
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
            let names =
                canonical_crew_pool(&pool.crews, self.context.settings().crews(), &pool.source)?;
            let crews = names
                .iter()
                .map(|name| self.resolve_crew_for_task(Some(name), None))
                .collect::<Result<Vec<_>, _>>()?;
            return Ok((crews, pool.source.clone()));
        }
        Ok((vec![self.effective_task_crew(task)?], "default".to_string()))
    }

    pub(crate) fn auto_task_crew_eligibility(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
        allowlist: &CrewAllowlist,
    ) -> Result<(), OrbitError> {
        let (crews, source) = self.auto_task_crew_candidates(task, pools, None)?;
        permitted_candidates(crews, &source, Some(allowlist)).map(|_| ())
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
        // Only auto coordinators and delivery pipelines carry this policy.
        // System, review and preparation jobs keep their own crew selection.
        if !matches!(
            job_name,
            "workspace_auto_pipeline"
                | "task_auto_pipeline"
                | "task_gate_pipeline"
                | "task_local_pipeline"
                | "task_pr_pipeline"
                | "epic_pipeline"
        ) {
            return Ok(());
        }
        if !input.is_object() {
            return Err(OrbitError::InvalidInput(
                "pipeline run input must be a JSON object".to_string(),
            ));
        }
        if job_name == "workspace_auto_pipeline" {
            input[POOLS_KEY] = json!(self.capture_auto_crew_pools(input)?);
            return Ok(());
        }
        let Some(parent_id) = parent_run_id else {
            return Ok(());
        };
        let Some(parent) = self.get_job_run_backend(parent_id)? else {
            return Ok(());
        };
        let Some(parent_input) = parent.input.as_ref() else {
            return Ok(());
        };
        let Some(pools_value) = parent_input.get(POOLS_KEY) else {
            return Ok(());
        };
        input[POOLS_KEY] = pools_value.clone();
        // Keep separately explicit constraints authoritative through every child.
        if let Some(allowed) = parent_input.get("allowed_crews") {
            input["allowed_crews"] = allowed.clone();
        }
        let Some(task_id) = auto_task_id(input).map(ToOwned::to_owned) else {
            return Ok(());
        };
        if let Some(selection) = parent_input.get(SELECTION_KEY)
            && selection.get("task_id").and_then(Value::as_str) == Some(&task_id)
        {
            input["crew"] = selection["crew"].clone();
            input[SELECTION_KEY] = selection.clone();
            return Ok(());
        }

        self.capture_auto_task_selection(input, &task_id, random)
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
        let index = random_index(candidates.len(), random)?;
        let selected = &candidates[index];
        input[SELECTION_KEY] = json!({
            "task_id": task.id,
            "crew": selected.name,
            "source": source,
            "complexity": task.complexity,
            "eligible_pool": candidates.iter().map(|crew| &crew.name).collect::<Vec<_>>(),
        });
        input["crew"] = json!(selected.name);
        Ok(())
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
    singular_task_id_from_input(input).or_else(|| {
        input
            .get("epic_task_id")
            .and_then(Value::as_str)
            .and_then(non_empty)
    })
}

fn permitted_candidates(
    crews: Vec<Crew>,
    source: &str,
    allowlist: Option<&CrewAllowlist>,
) -> Result<Vec<Crew>, OrbitError> {
    if crews.len() == 1 {
        enforce_crew_allowlist(allowlist, &crews[0], source)?;
        return Ok(crews);
    }
    let names = crews
        .iter()
        .map(|crew| crew.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let permitted = crews
        .into_iter()
        .filter(|crew| allowlist.is_none_or(|list| list.permits(crew)))
        .collect::<Vec<_>>();
    if permitted.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "crew pool from {source} [{names}] has no member permitted by this run's crew allowlist"
        )));
    }
    Ok(permitted)
}

fn random_index(
    len: usize,
    random: &mut impl FnMut() -> Result<u64, OrbitError>,
) -> Result<usize, OrbitError> {
    if len <= 1 {
        return Ok(0);
    }
    let bound = len as u64;
    // Rejection sampling avoids modulo bias for pools whose size is not a
    // power of two. No random draw occurs for explicit or singleton choices.
    let threshold = bound.wrapping_neg() % bound;
    loop {
        let ticket = random()?;
        if ticket >= threshold {
            return Ok((ticket % bound) as usize);
        }
    }
}

pub(crate) fn random_crew_ticket() -> Result<u64, OrbitError> {
    getrandom::u64().map_err(|error| OrbitError::Execution(format!("draw automatic crew: {error}")))
}
