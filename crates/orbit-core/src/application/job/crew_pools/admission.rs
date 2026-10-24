use orbit_common::OrbitError;
use orbit_config::ComplexityCrewPools;
use orbit_types::task::{Task, TaskComment, TaskComplexity, TaskHistoryEntry, TaskStatus};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::provider_limit::ProviderLimit;
use crate::application::task::{SYSTEM_ACTOR_LABEL, TaskRecordUpdateParams};
use crate::runtime::engine::crew::{CrewAllowlist, enforce_crew_allowlist};
use crate::runtime::run_input::non_empty;

use super::draw::{
    DrawWeighting, auto_task_id, permitted_candidates, pools_from_input, weighted_candidates,
    weighted_draw,
};
use super::types::{COMPLEXITIES, CapturedCrewPools, POLICY_PIPELINES, POOLS_KEY, SELECTION_KEY};
use super::{PROVIDER_LIMIT_REASSIGNMENT_PREFIX, auto_task_minted};

/// A planned provider-limit reassignment of an auto-task's explicit crew
/// [ORB-17638], decided before the admission draw.
struct ProviderLimitReassignment {
    original: String,
    provider: String,
    limit: String,
    complexity: TaskComplexity,
}

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
        // [ORB-17638] Only a task-level explicit crew qualifies; an explicit
        // run crew is the operator's call for this run alone.
        let reassignment = if explicit.is_none() {
            self.provider_limit_reassignment_plan(&task)
        } else {
            None
        };
        let (mut crews, mut source) = self.auto_task_crew_candidates(&task, &pools, explicit)?;
        // [ORB-17770] Weigh the narrowed pool by usage pressure and crew
        // performance; an explicit run crew is the operator's call and has
        // no draw to weigh. Performance never widens the pool: only the
        // usage-pressure view above re-draws an assigned crew.
        let weighting = if explicit.is_none() {
            weighted_candidates(
                &mut crews,
                &self.provider_limit_policy().usage_pressure,
                || self.usage_pressure_read(chrono::Utc::now()),
                || self.crew_performance_read(task.complexity, &task.tags, &task.context_files),
            )
        } else {
            None
        };
        if weighting.as_ref().is_some_and(DrawWeighting::pressured) {
            source = format!("{source}; usage pressure");
        }
        let allowlist = self.crew_allowlist_from_input(input)?;
        let candidates = permitted_candidates(crews, &source, allowlist.as_ref())?;
        let selected = weighted_draw(&candidates, &source, random)?;
        if let Some(weighting) = weighting.as_ref().filter(|weighting| weighting.pressured())
            && let Some(original) = task.crew.as_deref()
            && original != selected.crew.name
            && let Some(redraw) = self.usage_pressure_redraw_view(&task)?
            && let Some(automatic) = redraw.crew_source.as_deref()
            && let Err(error) = self.persist_usage_pressure_redraw(
                &task.id,
                original,
                automatic,
                &selected.crew.name,
                weighting,
            )
        {
            // As with a provider-limit reassignment, the draw for this run
            // stands and the next admission decides it again.
            tracing::warn!(task_id = %task.id, "could not record usage-pressure crew re-draw: {error}");
        }
        if let Some(plan) = &reassignment
            && plan.original != selected.crew.name
            && let Err(error) = self.persist_provider_limit_reassignment(
                &task.id,
                plan,
                &selected.crew.name,
                &pools,
            )
        {
            // The draw for this run stands; only the durable reassignment
            // is lost, and the next admission will decide it again.
            tracing::warn!(task_id = %task.id, "could not record provider-limit reassignment: {error}");
        }
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
            // A weighted draw adds each member's `pressure` [ORB-17770]
            // and `performance` factor, and the `effective_weight` it drew on.
            "eligible_pool": candidates
                .iter()
                .map(|candidate| match &weighting {
                    Some(weighting) => weighting.member(candidate),
                    None => json!({"name": candidate.crew.name, "weight": candidate.weight}),
                })
                .collect::<Vec<_>>(),
        });
        if let Some(limit) = provider_limit {
            input[SELECTION_KEY]["provider_limit"] = json!(limit);
        }
        input["crew"] = json!(selected.crew.name);
        Ok(())
    }

    /// The reassignment [ORB-17638] calls for on `task`, if any: an
    /// auto-task task, not yet running, whose explicit crew is gated by a
    /// live usage-limit reading. A `<provider>:off` override gates nothing
    /// in [`Self::provider_limit_gate`], so it plans nothing here either.
    fn provider_limit_reassignment_plan(&self, task: &Task) -> Option<ProviderLimitReassignment> {
        if !auto_task_minted(&task.tags)
            || !matches!(task.status, TaskStatus::Backlog | TaskStatus::Proposed)
        {
            return None;
        }
        match self.explicit_task_crew(task) {
            Ok(true) => {}
            Ok(false) => return None,
            Err(error) => {
                tracing::warn!(task_id = %task.id, "could not read crew provenance for provider-limit reassignment: {error}");
                return None;
            }
        }
        let complexity = task.complexity?;
        let original = task.crew.as_deref().and_then(non_empty)?.to_string();
        let crew = match self.resolve_crew_for_task(None, Some(&original)) {
            Ok(crew) => crew,
            Err(error) => {
                tracing::warn!(task_id = %task.id, "could not resolve explicit crew for provider-limit reassignment: {error}");
                return None;
            }
        };
        let gate = self.provider_limit_gate(chrono::Utc::now());
        let limit = gate.limit_for(&crew)?.describe();
        match self.provider_limit_reassignment_overridden(task) {
            Ok(true) => return None,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(task_id = %task.id, "could not read provider-limit reassignment history: {error}");
                return None;
            }
        }
        Some(ProviderLimitReassignment {
            provider: crew.assignment.provider.clone(),
            original,
            limit,
            complexity,
        })
    }

    /// Durably reassign `task_id` from its gated explicit crew to
    /// `replacement`, with the warning comment, as one locked record update
    /// [ORB-17638]. Revalidates under the task write lock: the crew must
    /// still be the gated one, the task must not be running or claimed, and
    /// no operator crew change may have superseded an earlier warning.
    /// `Ok(false)` when a revalidation refuses the write.
    fn persist_provider_limit_reassignment(
        &self,
        task_id: &str,
        plan: &ProviderLimitReassignment,
        replacement: &str,
        pools: &CapturedCrewPools,
    ) -> Result<bool, OrbitError> {
        // The replacement must be a drawable, non-gated member of the
        // task's complexity pool, not a purpose or default fallback.
        let Some((pool, _)) = self.complexity_pool_candidates(Some(plan.complexity), pools)? else {
            return Ok(false);
        };
        let Some(replacement_candidate) = pool
            .iter()
            .find(|candidate| candidate.weight > 0 && candidate.crew.name == replacement)
        else {
            return Ok(false);
        };
        let gate = self.provider_limit_gate(chrono::Utc::now());
        if gate.limit_for(&replacement_candidate.crew).is_some() {
            return Ok(false);
        }
        let warning = format!(
            "{PROVIDER_LIMIT_REASSIGNMENT_PREFIX} warning: crew `{}` (provider {}) is gated by {}; \
             reassigned to crew `{replacement}` from the {} complexity pool. \
             The task's explicit crew was replaced durably; a later operator crew change will not be overridden.",
            plan.original,
            plan.provider,
            plan.limit,
            plan.complexity.as_str(),
        );
        let mut recorded = false;
        self.stores().tasks().with_task_write_lock(task_id, &mut || {
            let current = self.get_task(task_id)?;
            if current.crew.as_deref() != Some(plan.original.as_str())
                || !matches!(current.status, TaskStatus::Backlog | TaskStatus::Proposed)
                || !auto_task_minted(&current.tags)
                || !self.explicit_task_crew(&current)?
                || self.provider_limit_reassignment_overridden(&current)?
            {
                return Ok(());
            }
            match self.inspect_execution_claims() {
                Ok(claims) if claims.iter().any(|inspection| {
                    inspection.claim.task_id == task_id && inspection.claim.phase.is_unsettled()
                }) => return Ok(()),
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(task_id = %task_id, "could not read execution claims for provider-limit reassignment: {error}");
                    return Ok(());
                }
            }
            let now = chrono::Utc::now();
            self.stores().task_records().update(
                task_id,
                TaskRecordUpdateParams {
                    actor: SYSTEM_ACTOR_LABEL.to_string(),
                    crew: Some(Some(replacement.to_string())),
                    crew_source: Some(Some(format!("pool:{}", plan.complexity.as_str()))),
                    expected_status: Some(vec![current.status]),
                    append_comments: vec![TaskComment {
                        at: now,
                        by: SYSTEM_ACTOR_LABEL.to_string(),
                        message: warning.clone(),
                        operation_id: None,
                    }],
                    append_history: vec![TaskHistoryEntry {
                        at: now,
                        by: SYSTEM_ACTOR_LABEL.to_string(),
                        event: "crew_assigned".to_string(),
                        note: Some(format!(
                            "assigned crew `{replacement}` from pool:{} (provider-limit reassignment from `{}`: {})",
                            plan.complexity.as_str(),
                            plan.original,
                            plan.limit,
                        )),
                        from_status: None,
                        to_status: None,
                        operation_id: None,
                        decision: None,
                    }],
                    ..Default::default()
                },
            )?;
            recorded = true;
            Ok(())
        })?;
        Ok(recorded)
    }

    /// Durably move `task_id` from `original` to `replacement`, the crew a
    /// usage-pressure admission re-draw chose, keeping its automatic
    /// `source` [ORB-17770]. Unlike [`Self::persist_provider_limit_reassignment`]
    /// the source is carried through, so a `purpose:` or `default` task is
    /// never relabelled as a pool draw. Revalidates under the task write
    /// lock that the crew and its source are still the ones drawn from.
    fn persist_usage_pressure_redraw(
        &self,
        task_id: &str,
        original: &str,
        source: &str,
        replacement: &str,
        weighting: &DrawWeighting,
    ) -> Result<(), OrbitError> {
        let now = chrono::Utc::now();
        let describe = |crew: &str| {
            weighting.pressure(crew).map_or_else(
                || "×1 (no live weekly usage)".to_string(),
                |pressure| pressure.describe(now),
            )
        };
        let mut note = format!(
            "crew redrawn from {source} (`{original}`) to {source} (`{replacement}`) at admission \
             under usage pressure: `{original}` {}, `{replacement}` {}",
            describe(original),
            describe(replacement),
        );
        // The re-draw names the performance factor only when it changed the
        // tickets. A shadow factor is recorded on the run and left out of
        // this note, because the draw did not use it.
        if weighting.performance_applied()
            && let Some((mode, _)) = weighting.performance_lens()
        {
            let performance = |crew: &str| {
                weighting
                    .performance_description(crew)
                    .unwrap_or_else(|| "×1".to_string())
            };
            note.push_str(&format!(
                "; crew performance {mode}: `{original}` {}, `{replacement}` {}",
                performance(original),
                performance(replacement),
            ));
        }
        self.stores()
            .tasks()
            .with_task_write_lock(task_id, &mut || {
                let current = self.get_task(task_id)?;
                if current.crew.as_deref() != Some(original)
                    || self.task_crew_source(&current)?.as_deref() != Some(source)
                {
                    return Ok(());
                }
                self.stores().task_records().update(
                    task_id,
                    TaskRecordUpdateParams {
                        actor: SYSTEM_ACTOR_LABEL.to_string(),
                        crew: Some(Some(replacement.to_string())),
                        crew_source: Some(Some(source.to_string())),
                        expected_status: Some(vec![current.status]),
                        append_history: vec![TaskHistoryEntry {
                            at: now,
                            by: SYSTEM_ACTOR_LABEL.to_string(),
                            event: "crew_redrawn".to_string(),
                            note: Some(note.clone()),
                            from_status: None,
                            to_status: None,
                            operation_id: None,
                            decision: None,
                        }],
                        ..Default::default()
                    },
                )?;
                Ok(())
            })
    }
}
