use orbit_common::OrbitError;
use orbit_config::{
    CrewPoolEntry, canonical_crew_pool, canonical_crew_pool_entries, executor_host_os,
};
use orbit_types::identity::Crew;
use orbit_types::task::{
    Task, TaskComplexity, TaskOsRequirement, TaskPriority, TaskStatus, TaskType,
};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::contracts::{
    CrewCandidate, INSPECTION_CREW_UNAVAILABLE, InspectionPool, PURPOSE_SOURCE_PREFIX,
    TYPE_SOURCE_PREFIX, admission_crew_source, automatic_crew_source, type_crew_source,
};
use crate::runtime::engine::crew::CrewAllowlist;
use crate::runtime::run_input::non_empty;

use super::auto_effort::{candidate_holds_assignment, merge_same_crew, task_auto_effort_subject};
use super::draw::{
    complexity_crews_key, permitted_candidates, pools_from_input, seeded_crew_ticket,
    sole_candidate, weighted_candidates, weighted_draw,
};
use super::purpose::task_crew_purpose;
use super::types::{COMPLEXITIES, CapturedCrewPool, CapturedCrewPools, POOLS_KEY};

impl OrbitRuntime {
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
                // A run override is admitted like the configured pool: an
                // `@auto` member needs bands for its crew.
                let policy = self.context.settings().auto_effort();
                if let Some(base) = pool.entries.iter().find_map(|entry| {
                    orbit_types::identity::auto_effort_base(&entry.name)
                        .filter(|base| policy.bands(base).is_none())
                }) {
                    return Err(OrbitError::InvalidInput(format!(
                        "{source} names '{base}@auto', but crew '{base}' has no bands; configure \
                         {}.{base} or name an effort",
                        orbit_config::AUTO_EFFORT_BANDS_KEY
                    )));
                }
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

    /// This host's crew pools by ordering key [ORB-17773]: each complexity
    /// tier, each configured `type:<type>/<tier>` pool and each configured
    /// `purpose:<purpose>` pool, naming its
    /// member crews. Admission ordering scores a candidate by the
    /// requester's reported pressure over these members. Best effort: a
    /// pool that cannot be read is omitted, which scores its candidates
    /// neutral rather than refusing an admission over an ordering hint.
    pub(crate) fn dispatch_crew_pools(&self) -> std::collections::BTreeMap<String, Vec<String>> {
        let mut pools = std::collections::BTreeMap::new();
        if let Ok(captured) = self.capture_auto_crew_pools(&Value::Null) {
            for (tier, pool) in captured {
                pools.insert(
                    tier,
                    pool.crews.into_iter().map(|entry| entry.name).collect(),
                );
            }
        }
        let settings = self.context.settings();
        for (task_type, tier, names) in settings.type_complexity_crews().iter() {
            let key = orbit_config::type_complexity_crews_key(task_type, tier);
            if let Ok(pool) = canonical_crew_pool(names, settings.crews(), &key) {
                pools.insert(
                    type_crew_source(task_type, tier),
                    pool.entries.into_iter().map(|entry| entry.name).collect(),
                );
            }
        }
        for purpose in orbit_types::workflow::CrewPurpose::ALL {
            let names = settings.purpose_crews().pool(purpose);
            if names.is_empty() {
                continue;
            }
            if let Ok(pool) = canonical_crew_pool(names, settings.crews(), purpose.config_key()) {
                pools.insert(
                    format!("purpose:{}", purpose.as_str()),
                    pool.entries.into_iter().map(|entry| entry.name).collect(),
                );
            }
        }
        pools
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
    ///
    /// A `<crew>@auto` member resolves against the task here
    /// ([`task_auto_effort_subject`]), whichever fallback produced it.
    pub(crate) fn auto_task_crew_candidates(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
        explicit: Option<&str>,
        allowlist: Option<&CrewAllowlist>,
    ) -> Result<(Vec<CrewCandidate>, String), OrbitError> {
        let (candidates, source) =
            self.auto_task_crew_candidates_unresolved(task, pools, explicit, allowlist)?;
        let candidates =
            self.reresolve_auto_candidates(candidates, &task_auto_effort_subject(task))?;
        Ok((candidates, source))
    }

    fn auto_task_crew_candidates_unresolved(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
        explicit: Option<&str>,
        allowlist: Option<&CrewAllowlist>,
    ) -> Result<(Vec<CrewCandidate>, String), OrbitError> {
        if explicit.and_then(non_empty).is_some() {
            return Ok((
                vec![sole_candidate(
                    self.resolve_crew_for_task(explicit, task.crew.as_deref())?,
                )],
                "explicit".to_string(),
            ));
        }
        let result = match self.usage_pressure_redraw_view(task)? {
            Some(redraw) => self.admissible_crew_candidates(&redraw, pools)?,
            None => self.admissible_crew_candidates(task, pools)?,
        };
        let Some(allowlist) = allowlist else {
            return Ok(result);
        };
        if result
            .0
            .iter()
            .any(|candidate| candidate.weight > 0 && allowlist.permits(&candidate.crew))
        {
            return Ok(result);
        }
        let Some(source) = self.task_crew_source(task)? else {
            return Ok(result);
        };
        let pool = if source == orbit_config::REVIEW_CREW_KEY {
            let policy = self.operation_policy();
            let pool = canonical_crew_pool(
                &policy.review_crew.value,
                self.context.settings().crews(),
                &source,
            )?;
            self.pool_member_candidates(&pool.entries, &source, None)?
        } else if source.starts_with(PURPOSE_SOURCE_PREFIX) {
            self.purpose_assignment_candidates(task)?
        } else if source.starts_with(TYPE_SOURCE_PREFIX) {
            self.type_assignment_candidates(task)?
        } else if let Some(tier) = source
            .strip_prefix("pool:")
            .and_then(|tier| tier.parse().ok())
        {
            self.complexity_pool_candidates(Some(tier), pools)?
        } else {
            None
        };
        let Some((candidates, pool_source)) = pool else {
            return Ok(result);
        };
        let mut candidates =
            self.reresolve_auto_candidates(candidates, &task_auto_effort_subject(task))?;
        let hold = self.admission_provider_hold(task);
        let gate = self.provider_limit_gate(chrono::Utc::now());
        candidates.retain(|candidate| {
            hold.as_ref()
                .is_none_or(|hold| !hold.excludes(&candidate.crew.name))
                && gate.limit_for(&candidate.crew).is_none()
        });
        // A redraw that offers no allowed crew leaves the hold and limit
        // fallbacks' own draw in place.
        if !candidates
            .iter()
            .any(|candidate| candidate.weight > 0 && allowlist.permits(&candidate.crew))
        {
            return Ok(result);
        }
        Ok((candidates, format!("{pool_source}; allowlist redraw")))
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

    /// The crew a claimed leaf runs as under this follower's own usage
    /// pressure [ORB-17773], or `None` to keep the owner's crew from the
    /// claimed-task snapshot.
    ///
    /// The re-draw is the [ORB-17770] admission re-draw over a view built
    /// from the snapshot (`crew`, its automatic `crew_source` and
    /// `complexity`): the whole pool its source names, as this follower
    /// captures that pool, narrowed by this host's holds and provider
    /// limits and by the run's crew allowlist, then weighed by this
    /// host's weekly readings. The owner's readings never reach it. An
    /// explicit or legacy crew (no automatic source), `static` policy, a
    /// draw with no pool to weigh, and any failure to
    /// compute the draw all keep the snapshot crew: a leaf that the owner
    /// admitted must not fail because an optional re-draw could not run.
    /// Nothing is persisted — the task lives in the owner's store — and
    /// the ticket is seeded from the claim, so every resolution of the
    /// leaf's crew in this run draws the same member.
    pub(crate) fn claimed_leaf_redrawn_crew(
        &self,
        input: &Value,
        task_id: &str,
        crew: Option<&str>,
        crew_source: Option<&str>,
        complexity: Option<TaskComplexity>,
    ) -> Option<String> {
        if !self.provider_limit_policy().usage_pressure.applies() {
            return None;
        }
        let original = crew.and_then(non_empty)?;
        let source = crew_source
            .and_then(non_empty)
            .filter(|source| automatic_crew_source(source))?;
        match self.claimed_leaf_redraw(input, task_id, original, source, complexity) {
            Ok(drawn) => drawn,
            Err(error) => {
                tracing::warn!(
                    task_id,
                    "could not re-draw the claimed task's crew under this host's usage \
                     pressure; keeping the owner's crew `{original}`: {error}"
                );
                None
            }
        }
    }

    fn claimed_leaf_redraw(
        &self,
        input: &Value,
        task_id: &str,
        original: &str,
        source: &str,
        complexity: Option<TaskComplexity>,
    ) -> Result<Option<String>, OrbitError> {
        // The claimed snapshot carries no tags or context files to classify
        // the task by, so a type-pool crew keeps the owner's draw.
        if source.starts_with(TYPE_SOURCE_PREFIX) {
            return Ok(None);
        }
        let pools = if input.get(POOLS_KEY).is_some() {
            pools_from_input(input)?
        } else {
            self.capture_auto_crew_pools(input)?
        };
        let now = chrono::Utc::now();
        let tags = source
            .strip_prefix(PURPOSE_SOURCE_PREFIX)
            .map(|purpose| {
                vec![format!(
                    "{}{purpose}",
                    orbit_types::workflow::CREW_PURPOSE_TAG_PREFIX
                )]
            })
            .unwrap_or_default();
        let view = Task {
            id: task_id.to_string(),
            title: String::new(),
            description: String::new(),
            acceptance_criteria: Vec::new(),
            tags,
            required_tools: Vec::new(),
            granted_tools: Vec::new(),
            requires_labels: Vec::new(),
            plan: String::new(),
            execution_summary: String::new(),
            context_files: Vec::new(),
            created_by: None,
            planned_by: None,
            implemented_by: None,
            status: TaskStatus::Backlog,
            priority: TaskPriority::Medium,
            complexity,
            task_type: TaskType::Chore,
            pr_status: None,
            external_refs: Vec::new(),
            relations: Vec::new(),
            job_run_id: None,
            job_run_machine: None,
            crew: Some(original.to_string()),
            crew_source: Some(source.to_string()),
            orchestrator: None,
            orchestrator_model: None,
            orchestrator_model_source: None,
            created_by_model: None,
            origin: None,
            rule_fingerprint: None,
            created_at: now,
            updated_at: now,
            not_before: None,
        };
        let Some(redraw) = self.usage_pressure_redraw_view(&view)? else {
            return Ok(None);
        };
        let (crews, draw_source) = self.admissible_claimed_leaf_crew_candidates(&redraw, &pools)?;
        let mut crews = self.reresolve_auto_candidates(crews, &task_auto_effort_subject(&view))?;
        // This re-draw weighs this host's usage pressure.
        let weighting = weighted_candidates(
            &mut crews,
            &self.provider_limit_policy().usage_pressure,
            || self.usage_pressure_read(chrono::Utc::now()),
        );
        if weighting.is_none() {
            return Ok(None);
        }
        let allowlist = self.crew_allowlist_from_input(input)?;
        let candidates = permitted_candidates(crews, &draw_source, allowlist.as_ref())?;
        let mut ticket = seeded_crew_ticket(format!("claimed:{task_id}:{source}:{original}"));
        let selected = weighted_draw(&candidates, &draw_source, &mut ticket)?;
        if selected.crew.name != original {
            tracing::info!(
                task_id,
                "crew redrawn from {source} (`{original}`) to {source} (`{}`) for the \
                 claimed leaf under this host's usage pressure",
                selected.crew.name
            );
        }
        Ok(Some(selected.crew.name.clone()))
    }

    /// The task's validated pool assignment or explicit pin, else its purpose
    /// pool, its type pool, its complexity pool, else the default chain,
    /// before any provider failure hold.
    pub(crate) fn unheld_task_crew_candidates(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
    ) -> Result<(Vec<CrewCandidate>, String), OrbitError> {
        self.unheld_task_crew_candidates_for(task, pools, true)
    }

    /// A claimed leaf's snapshot is owned by another host. Resolve its
    /// automatic pool against this host's current policy without asking this
    /// host's experiment store for an owner-side assignment.
    pub(in crate::application) fn unheld_claimed_leaf_crew_candidates(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
    ) -> Result<(Vec<CrewCandidate>, String), OrbitError> {
        self.unheld_task_crew_candidates_for(task, pools, false)
    }

    fn unheld_task_crew_candidates_for(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
        include_experiment: bool,
    ) -> Result<(Vec<CrewCandidate>, String), OrbitError> {
        // Each pool's `@auto` members resolve against the task before the
        // assigned member is looked for, so a task keeps its member while
        // the effort it resolves to moves.
        let subject = task_auto_effort_subject(task);
        let resolved = |found: Option<(Vec<CrewCandidate>, String)>| {
            found
                .map(|(candidates, source)| {
                    self.reresolve_auto_candidates(candidates, &subject)
                        .map(|candidates| (candidates, source))
                })
                .transpose()
        };
        if include_experiment && let Some(pool) = self.experiment_crew_pool(&task.id)? {
            return resolved(self.pool_member_candidates(&pool.entries, &pool.source, None)?)?
                .ok_or_else(|| {
                    OrbitError::PolicyDenied(format!(
                        "crew pool from {} has no enabled member that can run on this host",
                        pool.source
                    ))
                });
        }
        let source = self.task_crew_source(task)?;
        let pool_tier = source
            .as_deref()
            .and_then(|source| source.strip_prefix("pool:"));
        let purpose_source = source
            .as_deref()
            .and_then(|source| source.strip_prefix(PURPOSE_SOURCE_PREFIX));
        let type_source = source
            .as_deref()
            .filter(|source| source.starts_with(TYPE_SOURCE_PREFIX));
        // A crew admission wrote back for one delivery is not kept: the
        // task draws its pool again, as an automatic assignment.
        let automatic = pool_tier.is_some()
            || purpose_source.is_some()
            || type_source.is_some()
            || source.as_deref() == Some("default")
            || source.as_deref().is_some_and(admission_crew_source);
        let unassigned = self
            .crew_unless_retired(task.crew.as_deref())
            .and_then(non_empty)
            .is_none();
        // A purpose pool draws for a task whose crew the system chose (a pool,
        // purpose or default assignment) or that has none yet; an explicit
        // crew is kept as it is.
        if (unassigned || automatic)
            && let Some(purpose) = task_crew_purpose(&task.tags)
            && let Some((candidates, pool_source)) =
                resolved(self.purpose_pool_candidates(purpose, false)?)?
        {
            if purpose_source == Some(purpose.as_str())
                && let Some(candidate) = candidates.iter().find(|candidate| {
                    candidate.weight > 0
                        && candidate_holds_assignment(candidate, task.crew.as_deref())
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
        // The task's type pool for its tier draws next. A `type:` assignment
        // keeps its member while the task still classifies to that type and
        // tier and the member can run; a reclassified task draws its new pool.
        if (unassigned || automatic)
            && let Some(route) =
                self.task_type_pool_route(task.complexity, &task.tags, &task.context_files)
            && let Some((candidates, pool_source)) =
                resolved(self.type_pool_candidates(&route, None)?)?
        {
            if type_source == Some(route.crew_source().as_str())
                && let Some(candidate) = candidates.iter().find(|candidate| {
                    candidate.weight > 0
                        && candidate_holds_assignment(candidate, task.crew.as_deref())
                })
            {
                return Ok((
                    vec![sole_candidate(candidate.crew.clone())],
                    source.unwrap_or_default(),
                ));
            }
            return Ok((candidates, pool_source));
        }
        // A purpose or type assignment whose pool is gone routes like a
        // default one.
        if automatic {
            if let Some((candidates, pool_source)) =
                resolved(self.complexity_pool_candidates(task.complexity, pools)?)?
            {
                if task
                    .complexity
                    .is_some_and(|complexity| Some(complexity.as_str()) == pool_tier)
                    && let Some(candidate) = candidates.iter().find(|candidate| {
                        candidate.weight > 0
                            && candidate_holds_assignment(candidate, task.crew.as_deref())
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
        if let Some(drawn) = resolved(self.complexity_pool_candidates(task.complexity, pools)?)? {
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
        self.pool_member_candidates(&entries, &pool.source, task_os)
    }

    /// The enabled members of one complexity or type pool that this host
    /// (`task_os` `None`) or any serving host (`task_os` set, the creation
    /// filter) can run, or `None` when none remain.
    pub(super) fn pool_member_candidates(
        &self,
        entries: &[CrewPoolEntry],
        source: &str,
        task_os: Option<&TaskOsRequirement>,
    ) -> Result<Option<(Vec<CrewCandidate>, String)>, OrbitError> {
        let mut enabled = Vec::with_capacity(entries.len());
        for entry in entries {
            enabled.extend(self.pool_entry_candidate(&entry.name, entry.weight, None)?);
        }
        let enabled = merge_same_crew(enabled);
        if enabled.is_empty() {
            tracing::info!(
                source = %source,
                "every crew in the pool is disabled; routing to the next pool or workflow.default_crew",
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
                source = %source,
                "every crew in the pool is excluded by its executor host_os or task OS tags; routing to the next pool or workflow.default_crew",
            );
            return Ok(None);
        }
        Ok(Some((crews, source.to_string())))
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
            let (capable, unable) = self.partition_inspecting(
                self.configured_complexity_pool(tier)?
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|candidate| candidate.weight > 0)
                    .collect(),
            )?;
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
        if self.can_inspect(&crew)? {
            return Ok(crew);
        }
        Err(OrbitError::InvalidInput(format!(
            "{INSPECTION_CREW_UNAVAILABLE}: this activity inspects pinned source natively, but \
             no crew in `{}` or a higher tier, nor workflow.default_crew `{}` (provider `{}`), \
             has an executor that declares `inspection_args`; declare them on an executor whose \
             native file-read and shell tools work unattended, and add its crew to a pool",
            complexity_crews_key(tier),
            crew.name,
            crew.assignment.provider,
        )))
    }
}
