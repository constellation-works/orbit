use orbit_common::OrbitError;
use orbit_config::{canonical_crew_pool, canonical_crew_pool_entries, executor_host_os};
use orbit_types::identity::Crew;
use orbit_types::task::{Task, TaskComplexity, TaskHistoryEntry, TaskOsRequirement};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::runtime::run_input::non_empty;

use super::draw::{
    INSPECTION_CREW_UNAVAILABLE, can_inspect, complexity_crews_key, permitted_candidates,
    pools_from_input, pressure_weighted_candidates, random_crew_ticket, sole_candidate,
    weighted_draw,
};
use super::purpose::{PURPOSE_SOURCE_PREFIX, automatic_crew_source, task_crew_purpose};
use super::types::{
    COMPLEXITIES, CapturedCrewPool, CapturedCrewPools, CreationCrewAssignment, CrewCandidate,
    InspectionPool, POOLS_KEY,
};

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
            self.creation_crew_assignment(complexity, None, &task.tags, &mut random_crew_ticket)?;
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
            operation_id: None,
            decision: None,
        }))
    }

    pub(super) fn capture_auto_crew_pools(
        &self,
        input: &Value,
    ) -> Result<CapturedCrewPools, OrbitError> {
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

    /// The complexity pools a drain submitted now without overrides would
    /// capture, keyed by tier: each tier's weighted members and the
    /// `workflow.<tier>_complexity_crews` key they come from. The dashboard's
    /// window composer shows them as each tier's workspace choice.
    pub fn workspace_complexity_crew_pools(&self) -> Result<Value, OrbitError> {
        serde_json::to_value(self.capture_auto_crew_pools(&Value::Object(Default::default()))?)
            .map_err(|error| OrbitError::Store(error.to_string()))
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
    /// excludes from the draw, falling back to the task's purpose pool, then
    /// the complexity pool, then the default. [ORB-14697] This host's provider
    /// limits then remove the limited crews, falling back to that purpose pool
    /// and then the complexity pool only. An explicit crew is the caller's
    /// decision and ignores both.
    ///
    /// [ORB-17770] Under usage pressure, a crew the system chose is drawn
    /// again from its whole pool ([`Self::usage_pressure_redraw_view`]).
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
        match self.usage_pressure_redraw_view(task)? {
            Some(redraw) => self.admissible_crew_candidates(&redraw, pools),
            None => self.admissible_crew_candidates(task, pools),
        }
    }

    /// `task` as an admission re-draw sees it [ORB-17770]: with no crew and
    /// its automatic source (`pool:<tier>`, `purpose:<purpose>` or
    /// `default`), so the draw covers the source's whole pool instead of
    /// keeping the assigned member. `None` when usage pressure does not
    /// apply, the task has no crew, or its crew is an explicit choice or a
    /// legacy pin, which are never re-drawn.
    pub(crate) fn usage_pressure_redraw_view(
        &self,
        task: &Task,
    ) -> Result<Option<Task>, OrbitError> {
        if !self.provider_limit_policy().usage_pressure.applies()
            || self
                .crew_unless_retired(task.crew.as_deref())
                .and_then(non_empty)
                .is_none()
        {
            return Ok(None);
        }
        let Some(source) = self
            .task_crew_source(task)?
            .filter(|source| automatic_crew_source(source))
        else {
            return Ok(None);
        };
        let mut redraw = task.clone();
        redraw.crew = None;
        redraw.crew_source = Some(source);
        Ok(Some(redraw))
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
        let purpose_source = source
            .as_deref()
            .and_then(|source| source.strip_prefix(PURPOSE_SOURCE_PREFIX));
        let unassigned = self
            .crew_unless_retired(task.crew.as_deref())
            .and_then(non_empty)
            .is_none();
        // A purpose pool draws for a task whose crew the system chose (a pool,
        // purpose or default assignment) or that has none yet; an explicit
        // crew is kept as it is.
        if (unassigned
            || pool_tier.is_some()
            || purpose_source.is_some()
            || source.as_deref() == Some("default"))
            && let Some(purpose) = task_crew_purpose(&task.tags)
            && let Some((candidates, pool_source)) = self.purpose_pool_candidates(purpose, false)?
        {
            if purpose_source == Some(purpose.as_str())
                && let Some(candidate) = candidates.iter().find(|candidate| {
                    candidate.weight > 0
                        && Some(candidate.crew.name.as_str()) == task.crew.as_deref()
                })
            {
                // Keep the assigned member while it can run. A hold or a usage
                // limit retries the rest of this pool before the complexity
                // pool; collapsing here is what makes that retry necessary.
                return Ok((
                    vec![sole_candidate(candidate.crew.clone())],
                    source.unwrap_or_default(),
                ));
            }
            return Ok((candidates, pool_source));
        }
        // A purpose assignment whose pool is gone routes like a default one.
        if pool_tier.is_some() || purpose_source.is_some() || source.as_deref() == Some("default") {
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
        // A task minted with the retired `system` crew routes like an
        // unassigned one [ORB-15663].
        if let Some(crew) = self
            .crew_unless_retired(task.crew.as_deref())
            .and_then(non_empty)
        {
            return Ok((
                vec![sole_candidate(
                    self.resolve_crew_for_task(None, Some(crew))?,
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
        self.complexity_pool_candidates_on(complexity, pools, None)
    }

    /// The creation-time counterpart of [`Self::complexity_pool_candidates`]:
    /// a crew counts when any host serving with this one can run its
    /// executor and its OS limit intersects the task's tags, so an owner
    /// whose pool holds a follower-only crew can draw
    /// it and let the task wait for that follower. Only creation and
    /// complexity re-rates draw this way; admission, provider holds and
    /// activity fallbacks keep the executing-host filter above.
    pub(crate) fn creation_complexity_pool_candidates(
        &self,
        complexity: Option<TaskComplexity>,
        pools: &CapturedCrewPools,
        task_os: &TaskOsRequirement,
    ) -> Result<Option<(Vec<CrewCandidate>, String)>, OrbitError> {
        self.complexity_pool_candidates_on(complexity, pools, Some(task_os))
    }

    fn complexity_pool_candidates_on(
        &self,
        complexity: Option<TaskComplexity>,
        pools: &CapturedCrewPools,
        task_os: Option<&TaskOsRequirement>,
    ) -> Result<Option<(Vec<CrewCandidate>, String)>, OrbitError> {
        let Some(pool) = complexity
            .and_then(|complexity| pools.get(complexity.as_str()))
            .filter(|pool| !pool.crews.is_empty())
        else {
            return Ok(None);
        };
        let registry = self.context.settings().crews();
        let entries = canonical_crew_pool_entries(&pool.crews, registry, &pool.source)?;
        let enabled = entries
            .iter()
            .filter(|entry| registry.get(&entry.name).is_some_and(|crew| crew.enabled))
            .map(|entry| {
                Ok(CrewCandidate {
                    crew: self.resolve_crew_for_task(Some(&entry.name), None)?,
                    weight: entry.weight,
                })
            })
            .collect::<Result<Vec<_>, OrbitError>>()?;
        if enabled.is_empty() {
            tracing::info!(
                source = %pool.source,
                "every crew in the pool is disabled; routing to workflow.default_crew",
            );
            return Ok(None);
        }
        // A crew whose executor `host_os` excludes this host is not drawn, so
        // admission does not select it and then withhold it. The creation
        // draw (`task_os`) instead keeps a crew any serving host can
        // run: the assigned task waits for that host rather than being
        // redrawn here. Crews with no declaration stay, with their weights,
        // so an unconstrained pool draws as before. An explicit crew never
        // reaches this draw.
        let declared = self.declared_executor_host_os()?;
        // Resolve the serving set once: it reads this workspace's claim
        // records, which no candidate's answer can change.
        let serving = task_os.map(|_| self.serving_host_oses());
        let crews = enabled
            .into_iter()
            .filter(|candidate| {
                task_os.is_none_or(|requirement| {
                    requirement
                        .intersect_executor(
                            &candidate.crew.name,
                            &candidate.crew.assignment.provider,
                            executor_host_os(&candidate.crew.assignment.provider, &declared),
                        )
                        .is_ok()
                })
            })
            .filter(|candidate| match &serving {
                Some(oses) => self.crew_runs_on_hosts(&candidate.crew, &declared, oses),
                None => self.crew_runs_on_host(&candidate.crew, &declared),
            })
            .collect::<Vec<_>>();
        if crews.is_empty() {
            tracing::info!(
                source = %pool.source,
                "every crew in the pool is excluded by its executor host_os or task OS tags; routing to workflow.default_crew",
            );
            return Ok(None);
        }
        Ok(Some((crews, pool.source.clone())))
    }

    /// The enabled members of `workflow.<tier>_complexity_crews` as the
    /// current configuration declares them, or `None` when that pool is
    /// unset, empty or wholly disabled.
    pub(crate) fn configured_complexity_pool(
        &self,
        tier: TaskComplexity,
    ) -> Result<Option<Vec<CrewCandidate>>, OrbitError> {
        let pools = self.capture_auto_crew_pools(&Value::Null)?;
        Ok(self
            .complexity_pool_candidates(Some(tier), &pools)?
            .map(|(candidates, _)| candidates))
    }

    /// The pool an inspection-requiring draw for `tier` uses [ORB-15792].
    ///
    /// A pool whose enabled members cannot inspect, or hold no ticket, passes
    /// to the next tier up (low → medium → hard → xhard). `None` when no tier
    /// from `tier` upward has an inspection-capable member, which sends the
    /// caller to [`Self::inspection_capable_default_crew`].
    pub(crate) fn inspection_capable_pool(
        &self,
        tier: TaskComplexity,
    ) -> Result<Option<InspectionPool>, OrbitError> {
        let mut skipped = Vec::new();
        for tier in COMPLEXITIES.into_iter().skip_while(|each| *each != tier) {
            let (capable, unable): (Vec<_>, Vec<_>) = self
                .configured_complexity_pool(tier)?
                .unwrap_or_default()
                .into_iter()
                .filter(|candidate| candidate.weight > 0)
                .partition(|candidate| can_inspect(&candidate.crew));
            skipped.extend(unable.into_iter().map(|candidate| candidate.crew.name));
            if !capable.is_empty() {
                return Ok(Some(InspectionPool {
                    tier,
                    candidates: capable,
                    skipped,
                }));
            }
        }
        Ok(None)
    }

    /// `workflow.default_crew` for an inspection-requiring activity whose
    /// pools from `tier` upward hold no inspection-capable member. Refused
    /// with [`INSPECTION_CREW_UNAVAILABLE`] before dispatch when it cannot
    /// inspect either [ORB-15792].
    pub(crate) fn inspection_capable_default_crew(
        &self,
        tier: TaskComplexity,
    ) -> Result<Crew, OrbitError> {
        let crew = self.resolve_crew_for_task(None, None)?;
        if can_inspect(&crew) {
            return Ok(crew);
        }
        Err(OrbitError::InvalidInput(format!(
            "{INSPECTION_CREW_UNAVAILABLE}: this activity inspects pinned source natively, but \
             no crew in `{}` or a higher tier, nor workflow.default_crew `{}` (provider `{}`), \
             has a native file-read and command surface; add a Codex or Claude crew to a pool",
            complexity_crews_key(tier),
            crew.name,
            crew.assignment.provider,
        )))
    }

    /// Decide the crew a task is created with [ORB-12717].
    ///
    /// An explicit crew wins; then the pool of the purpose the task's tags
    /// name, when one is configured (source `purpose:<purpose>`).
    /// Creation and complexity re-rates draw from the complexity pools;
    /// a status transition alone never revisits the choice, and
    /// `task update --crew ""` re-enters here for the task's current
    /// complexity. Complexity pools exclude crews incompatible with the tags
    /// this write leaves the task with, before drawing. An explicit crew is
    /// kept exactly as the caller wrote it.
    /// [ORB-17770] Under usage pressure, both pool draws weigh their members
    /// by crew pressure; this covers task add, `task update --crew ""` and
    /// [`Self::rerate_task_crew`]. The recorded source stays the pool's.
    /// `None` means this workspace can name no crew at all, so the field stays
    /// unset and dispatch resolves one from configuration as it always has.
    pub(crate) fn creation_crew_assignment(
        &self,
        complexity: Option<TaskComplexity>,
        explicit: Option<&str>,
        tags: &[String],
        random: &mut impl FnMut() -> Result<u64, OrbitError>,
    ) -> Result<Option<CreationCrewAssignment>, OrbitError> {
        if let Some(explicit) = explicit.and_then(non_empty) {
            return Ok(Some(CreationCrewAssignment {
                crew: explicit.to_string(),
                source: "explicit".to_string(),
            }));
        }
        if let Some(purpose) = task_crew_purpose(tags)
            && let Some((candidates, _)) = self.purpose_pool_candidates(purpose, false)?
        {
            let source = format!("{PURPOSE_SOURCE_PREFIX}{purpose}");
            let candidates = self.creation_pressure_weighted(candidates);
            let candidates = permitted_candidates(candidates, &source, None)?;
            let selected = weighted_draw(&candidates, &source, random)?;
            return Ok(Some(CreationCrewAssignment {
                crew: selected.crew.name.clone(),
                source,
            }));
        }
        let pools = self.capture_auto_crew_pools(&Value::Null)?;
        if let Some(complexity) = complexity
            && let Some((candidates, _)) = self.creation_complexity_pool_candidates(
                Some(complexity),
                &pools,
                &TaskOsRequirement::from_tags(tags),
            )?
        {
            let source = format!("pool:{complexity}");
            let candidates = self.creation_pressure_weighted(candidates);
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
        // A default no serving host can run is not pinned: the field stays
        // unset rather than assigning a crew every host would withhold. A
        // default only a follower can run is pinned, like a creation pool
        // draw, and waits for that follower. An explicit crew is returned
        // above and is never replaced.
        let declared = self.declared_executor_host_os()?;
        if !self.crew_runs_on_serving_host(&default, &declared) {
            return Ok(None);
        }
        Ok(Some(CreationCrewAssignment {
            crew: default.name,
            source: "default".to_string(),
        }))
    }

    /// A creation-seam pool draw's members, weighed by usage pressure when
    /// it applies [ORB-17770].
    fn creation_pressure_weighted(&self, mut candidates: Vec<CrewCandidate>) -> Vec<CrewCandidate> {
        pressure_weighted_candidates(
            &mut candidates,
            &self.provider_limit_policy().usage_pressure,
            || self.usage_pressure_read(chrono::Utc::now()),
        );
        candidates
    }
}
