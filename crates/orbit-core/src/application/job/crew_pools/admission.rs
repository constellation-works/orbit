use orbit_common::OrbitError;
use orbit_config::ComplexityCrewPools;
use orbit_types::task::Task;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::provider_limit::ProviderLimit;
use crate::runtime::engine::crew::{CrewAllowlist, enforce_crew_allowlist};
use crate::runtime::run_input::non_empty;

use super::draw::{auto_task_id, permitted_candidates, pools_from_input, weighted_draw};
use super::types::{COMPLEXITIES, POLICY_PIPELINES, POOLS_KEY, SELECTION_KEY};

/// Replay recaptures automatic admission under current policy. Only a crew
/// recorded as the caller's explicit choice survives; run-level pool overrides
/// and allowlists remain caller input. Other jobs do not use this admission.
pub(crate) fn strip_auto_crew_admission(job_name: &str, input: &mut Value) {
    if !POLICY_PIPELINES.contains(&job_name) {
        return;
    }
    if let Some(object) = input.as_object_mut() {
        let explicit = object
            .get(SELECTION_KEY)
            .and_then(|selection| selection.get("source"))
            .and_then(Value::as_str)
            == Some("explicit");
        object.remove(SELECTION_KEY);
        object.remove(POOLS_KEY);
        if !explicit {
            object.remove("crew");
        }
    }
}

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

    /// Discovery uses only a durable selection for this task, never a crew
    /// belonging to a sibling or an unadmitted activity input override.
    pub(crate) fn admitted_task_crew_for_input(
        &self,
        input: &Value,
        task_id: &str,
    ) -> Result<Option<String>, OrbitError> {
        let Some(run_id) = input
            .get("run_id")
            .and_then(Value::as_str)
            .and_then(non_empty)
        else {
            return Ok(None);
        };
        let Some(run_input) = self.get_job_run_backend(run_id)?.and_then(|run| run.input) else {
            return Ok(None);
        };
        Ok(run_input
            .get(SELECTION_KEY)
            .filter(|selection| selection.get("task_id").and_then(Value::as_str) == Some(task_id))
            .and_then(|selection| selection.get("crew"))
            .and_then(Value::as_str)
            .and_then(non_empty)
            .map(ToOwned::to_owned))
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
        if let [only] = candidates.as_slice()
            && only.weight > 0
        {
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
        // [ORB-14697] An explicit run crew is not gated; the selection
        // records the provider limit it runs against.
        let provider_limit = explicit.and_then(|_| {
            self.provider_limit_gate(chrono::Utc::now())
                .limit_for(&selected.crew)
                .map(ProviderLimit::describe)
        });
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
        if let Some(limit) = provider_limit {
            input[SELECTION_KEY]["provider_limit"] = json!(limit);
        }
        input["crew"] = json!(selected.crew.name);
        Ok(())
    }
}
