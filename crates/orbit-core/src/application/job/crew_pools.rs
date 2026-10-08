//! Draw a task's crew from the complexity pools when it is created
//! [ORB-12717], and capture pool policy on the admitting pipeline so each
//! task's selection is frozen in its run input. Descendant pipelines inherit
//! the same task choice; a task created without a crew before assignment moved
//! to creation time, a stale pool assignment after rerating, or a default
//! fallback for an empty pool, is routed through the current pools at admission,
//! which reads the record and never writes it.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_config::{
    ComplexityCrewPools, CrewPoolEntry, canonical_crew_pool, canonical_crew_pool_entries,
};
use orbit_types::identity::Crew;
use orbit_types::task::{Task, TaskComplexity, TaskHistoryEntry};
use orbit_types::workflow::RunStateUpdate;
use orbit_types::workflow::{ActivityCrewDraw, ActivityCrewPoolMember, FINAL_RECOVERY_CREWS_KEY};
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

/// Crew chosen for a task at creation, with the provenance its history entry
/// records: `explicit`, `pool:<complexity>`, or `default` [ORB-12717].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CreationCrewAssignment {
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
    /// Recover legacy assignment provenance only from the latest assignment
    /// that names the current crew. Missing evidence preserves an explicit pin.
    pub(crate) fn task_crew_source(&self, task: &Task) -> Result<Option<String>, OrbitError> {
        if task.crew_source.is_some() {
            return Ok(task.crew_source.clone());
        }
        let Some(crew) = task.crew.as_deref() else {
            return Ok(None);
        };
        let history = self.get_task_history(&task.id)?;
        let Some(note) = history
            .iter()
            .rev()
            .find(|entry| matches!(entry.event.as_str(), "crew_assigned" | "crew_redrawn"))
            .and_then(|entry| entry.note.as_deref())
        else {
            return Ok(None);
        };
        if let Some(source) = note.strip_prefix(&format!("assigned crew `{crew}` from ")) {
            return Ok(Some(source.to_string()));
        }
        if let Some((_, source)) = note.split_once(&format!(" to `{crew}` via ")) {
            return Ok(if source == "explicit name" {
                Some("explicit".to_string())
            } else {
                source
                    .strip_prefix("pool draw (")
                    .and_then(|source| source.strip_suffix(')'))
                    .map(ToOwned::to_owned)
            });
        }
        Ok(None)
    }

    /// Redraw a pool assignment from another tier or a default fallback before
    /// publishing the new complexity. The caller commits crew, source and this
    /// history together.
    pub(crate) fn rerate_task_crew(
        &self,
        task: &mut Task,
        complexity: Option<TaskComplexity>,
    ) -> Result<Option<TaskHistoryEntry>, OrbitError> {
        let Some(source) = self.task_crew_source(task)? else {
            return Ok(None);
        };
        let pool_tier = source.strip_prefix("pool:");
        if source != "default" && pool_tier.is_none() {
            return Ok(None);
        }
        if (source == "default" && complexity == task.complexity)
            || complexity.is_some_and(|complexity| Some(complexity.as_str()) == pool_tier)
        {
            return Ok(None);
        }
        let before = task.crew.clone();
        let assignment =
            self.creation_crew_assignment(complexity, None, &mut random_crew_ticket)?;
        task.crew = assignment
            .as_ref()
            .map(|assignment| assignment.crew.clone());
        task.crew_source = assignment.map(|assignment| assignment.source);
        Ok(Some(TaskHistoryEntry {
            at: chrono::Utc::now(),
            by: "system".to_string(),
            event: "crew_redrawn".to_string(),
            note: Some(format!(
                "crew redrawn from {} (`{}`) to {} (`{}`) after complexity changed to {}",
                source,
                before.as_deref().unwrap_or("(none)"),
                task.crew_source.as_deref().unwrap_or("unassigned"),
                task.crew.as_deref().unwrap_or("(none)"),
                complexity.map_or("unassessed", TaskComplexity::as_str),
            )),
            from_status: None,
            to_status: None,
        }))
    }

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
    ///
    /// [ORB-14266] A standing provider failure hold removes the crews it
    /// excludes from the draw, falling back to the complexity pool and then
    /// the default. An explicit crew is the caller's decision and ignores it.
    pub(crate) fn auto_task_crew_candidates(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
        explicit: Option<&str>,
    ) -> Result<(Vec<CrewCandidate>, String), OrbitError> {
        if explicit.and_then(non_empty).is_some() {
            return Ok((
                vec![sole_candidate(
                    self.resolve_crew_for_task(explicit, task.crew.as_deref())?,
                )],
                "explicit".to_string(),
            ));
        }
        let (candidates, source) = self.unheld_task_crew_candidates(task, pools)?;
        self.apply_provider_hold(task, pools, candidates, source)
    }

    /// The task's validated pool assignment or explicit pin, else its complexity
    /// pool, else the default chain,
    /// before any provider failure hold.
    pub(crate) fn unheld_task_crew_candidates(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
    ) -> Result<(Vec<CrewCandidate>, String), OrbitError> {
        let source = self.task_crew_source(task)?;
        let pool_tier = source
            .as_deref()
            .and_then(|source| source.strip_prefix("pool:"));
        if pool_tier.is_some() || source.as_deref() == Some("default") {
            if let Some((candidates, pool_source)) =
                self.complexity_pool_candidates(task.complexity, pools)?
            {
                if task
                    .complexity
                    .is_some_and(|complexity| Some(complexity.as_str()) == pool_tier)
                    && let Some(candidate) = candidates.iter().find(|candidate| {
                        candidate.weight > 0
                            && Some(candidate.crew.name.as_str()) == task.crew.as_deref()
                    })
                {
                    return Ok((
                        vec![sole_candidate(candidate.crew.clone())],
                        source.unwrap_or_default(),
                    ));
                }
                return Ok((candidates, pool_source));
            }
            return Ok((
                vec![sole_candidate(self.resolve_crew_for_task(None, None)?)],
                "default".to_string(),
            ));
        }
        if task.crew.as_deref().and_then(non_empty).is_some() {
            return Ok((
                vec![sole_candidate(
                    self.resolve_crew_for_task(None, task.crew.as_deref())?,
                )],
                "task.crew".to_string(),
            ));
        }
        if let Some(drawn) = self.complexity_pool_candidates(task.complexity, pools)? {
            return Ok(drawn);
        }
        Ok((
            vec![sole_candidate(self.effective_task_crew(task)?)],
            "default".to_string(),
        ))
    }

    /// The enabled pool members configured for `complexity`, with the
    /// provenance the pool was captured from. `None` when no pool covers the
    /// complexity, which is what sends both callers to the default chain.
    ///
    /// A disabled crew (`[crews.<name>] enabled = false`) is never drawn.
    /// A pool whose members are all disabled is treated exactly like an empty
    /// pool and also returns `None`, so the task falls through to
    /// `workflow.default_crew` — and dispatch refuses that too if it is
    /// disabled. Enabled state is read from the current configuration, not the
    /// captured pool, so disabling a crew takes effect for the next draw.
    pub(crate) fn complexity_pool_candidates(
        &self,
        complexity: Option<TaskComplexity>,
        pools: &CapturedCrewPools,
    ) -> Result<Option<(Vec<CrewCandidate>, String)>, OrbitError> {
        let Some(pool) = complexity
            .and_then(|complexity| pools.get(complexity.as_str()))
            .filter(|pool| !pool.crews.is_empty())
        else {
            return Ok(None);
        };
        let registry = self.context.settings().crews();
        let entries = canonical_crew_pool_entries(&pool.crews, registry, &pool.source)?;
        let crews = entries
            .iter()
            .filter(|entry| registry.get(&entry.name).is_some_and(|crew| crew.enabled))
            .map(|entry| {
                Ok(CrewCandidate {
                    crew: self.resolve_crew_for_task(Some(&entry.name), None)?,
                    weight: entry.weight,
                })
            })
            .collect::<Result<Vec<_>, OrbitError>>()?;
        if crews.is_empty() {
            tracing::info!(
                source = %pool.source,
                "every crew in the pool is disabled; routing to workflow.default_crew",
            );
            return Ok(None);
        }
        Ok(Some((crews, pool.source.clone())))
    }

    /// Decide the crew a task is created with [ORB-12717].
    ///
    /// Creation and complexity re-rates draw from the complexity pools;
    /// a status transition alone never revisits the choice, and
    /// `task update --crew ""` re-enters here for the task's current
    /// complexity. An explicit crew is kept exactly as the caller wrote it.
    /// `None` means this workspace can name no crew at all, so the field stays
    /// unset and dispatch resolves one from configuration as it always has.
    pub(crate) fn creation_crew_assignment(
        &self,
        complexity: Option<TaskComplexity>,
        explicit: Option<&str>,
        random: &mut impl FnMut() -> Result<u64, OrbitError>,
    ) -> Result<Option<CreationCrewAssignment>, OrbitError> {
        if let Some(explicit) = explicit.and_then(non_empty) {
            return Ok(Some(CreationCrewAssignment {
                crew: explicit.to_string(),
                source: "explicit".to_string(),
            }));
        }
        let pools = self.capture_auto_crew_pools(&Value::Null)?;
        if let Some(complexity) = complexity
            && let Some((candidates, _)) =
                self.complexity_pool_candidates(Some(complexity), &pools)?
        {
            let source = format!("pool:{complexity}");
            let candidates = permitted_candidates(candidates, &source, None)?;
            let selected = weighted_draw(&candidates, &source, random)?;
            return Ok(Some(CreationCrewAssignment {
                crew: selected.crew.name.clone(),
                source,
            }));
        }
        if self.context.settings().default_crew().is_none() {
            return Ok(None);
        }
        // A disabled default is not pinned onto the task: creation stays
        // possible on a host with no enabled crew, and the unset field lets
        // dispatch refuse against `workflow.default_crew` by name.
        let default = self.lookup_crew_for_task(None, None)?;
        if !default.enabled {
            return Ok(None);
        }
        Ok(Some(CreationCrewAssignment {
            crew: default.name,
            source: "default".to_string(),
        }))
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
}

impl OrbitRuntime {
    /// The crew a `final_recovery` activity runs as: one draw from
    /// `workflow.final_recovery_crews`, frozen in the executing run's state.
    /// Dispatch supplies that identity as `job_run_id` when an explicit
    /// `run_id` names the originating failure; otherwise `run_id` is the
    /// executing run. A follower's originating state need not exist here.
    ///
    /// The first dispatch draws over the enabled members this run's
    /// `allowed_crews` permits and records the choice in the same run-state
    /// transaction that reads it, so two racing dispatches cannot record
    /// different crews. Every later dispatch of the key in this run, and every
    /// resume seeded from it, reuses the record even if the pool has since
    /// changed; the caller's allowlist gate still applies to the frozen crew.
    pub(crate) fn final_recovery_crew(
        &self,
        input: &Value,
        random: &mut impl FnMut() -> Result<u64, OrbitError>,
    ) -> Result<Crew, OrbitError> {
        let source = FINAL_RECOVERY_CREWS_KEY;
        let run_id = input
            .get("job_run_id")
            .or_else(|| input.get("run_id"))
            .and_then(Value::as_str)
            .and_then(non_empty)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "an activity crew drawn from `{source}` needs the input's executing \
                     `job_run_id` or `run_id` to freeze the draw"
                ))
            })?;
        let allowlist = self.crew_allowlist_from_input(input)?;
        let mut drawn: Option<ActivityCrewDraw> = None;
        let update = self
            .stores()
            .jobs()
            .update_run_state(run_id, &mut |_, state| {
                if let Some(frozen) = state.activity_crew_draws.get(source) {
                    drawn = Some(frozen.clone());
                    return Ok(());
                }
                let candidates = self.final_recovery_candidates()?;
                let candidates = permitted_candidates(candidates, source, allowlist.as_ref())?;
                let selected = weighted_draw(&candidates, source, random)?;
                let draw = ActivityCrewDraw {
                    crew: selected.crew.name.clone(),
                    source: source.to_string(),
                    eligible_pool: candidates
                        .iter()
                        .map(|candidate| ActivityCrewPoolMember {
                            name: candidate.crew.name.clone(),
                            weight: candidate.weight,
                        })
                        .collect(),
                };
                state
                    .activity_crew_draws
                    .insert(source.to_string(), draw.clone());
                drawn = Some(draw);
                Ok(())
            })?;
        if update != RunStateUpdate::Updated {
            return Err(OrbitError::InvalidInput(format!(
                "run '{run_id}' has no persisted state to freeze its `{source}` draw in"
            )));
        }
        let draw = drawn.ok_or_else(|| {
            OrbitError::Execution(format!(
                "`{source}` draw for run '{run_id}' did not run inside its state update"
            ))
        })?;
        self.resolve_crew_for_task(Some(&draw.crew), None)
    }

    /// The enabled members of `workflow.final_recovery_crews`, each with its
    /// tickets. An empty pool disables final recovery, and a pool whose every
    /// member is disabled is refused the same way rather than falling back to
    /// another crew.
    fn final_recovery_candidates(&self) -> Result<Vec<CrewCandidate>, OrbitError> {
        let source = FINAL_RECOVERY_CREWS_KEY;
        let registry = self.context.settings().crews();
        let pool = canonical_crew_pool(
            self.context.settings().final_recovery_crews(),
            registry,
            source,
        )?;
        if pool.entries.is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "final recovery is disabled: `{source}` is []"
            )));
        }
        let candidates = pool
            .entries
            .iter()
            .filter(|entry| registry.get(&entry.name).is_some_and(|crew| crew.enabled))
            .map(|entry| {
                Ok(CrewCandidate {
                    crew: self.resolve_crew_for_task(Some(&entry.name), None)?,
                    weight: entry.weight,
                })
            })
            .collect::<Result<Vec<_>, OrbitError>>()?;
        if candidates.is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "every crew in `{source}` is disabled"
            )));
        }
        Ok(candidates)
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
fn weighted_draw<'a>(
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
