use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_config::canonical_crew_pool;
use orbit_types::identity::Crew;
use orbit_types::task::TaskComplexity;
use orbit_types::workflow::activity_job::requires_native_inspection;
use orbit_types::workflow::{ActivityCrewDraw, FINAL_RECOVERY_CREWS_KEY, RunStateUpdate};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::distributed::HostProviderHold;
use crate::runtime::engine::crew::CrewAllowlist;
use crate::runtime::run_input::non_empty;

use super::draw::{
    activity_crew_draw, complexity_crews_key, permitted_candidates, sole_candidate, weighted_draw,
};
use super::types::CrewCandidate;

impl OrbitRuntime {
    /// The crew a `final_recovery` activity runs as: one draw from
    /// `workflow.final_recovery_crews`, frozen in the executing run's state.
    ///
    /// The first dispatch draws over the enabled members this run's
    /// `allowed_crews` permits. Every later dispatch of the key in this run,
    /// and every resume seeded from it, reuses the record even if the pool
    /// has since changed; the caller's allowlist gate still applies to the
    /// frozen crew.
    pub(crate) fn final_recovery_crew(
        &self,
        input: &Value,
        random: &mut impl FnMut() -> Result<u64, OrbitError>,
    ) -> Result<Crew, OrbitError> {
        let source = FINAL_RECOVERY_CREWS_KEY;
        self.frozen_activity_crew(input, source, &mut |allowlist| {
            let candidates = self.final_recovery_candidates()?;
            let candidates = permitted_candidates(candidates, source, allowlist)?;
            let selected = weighted_draw(&candidates, source, random)?;
            Ok(activity_crew_draw(selected, source, &candidates))
        })
    }

    /// The crew system work without a task runs as [ORB-15663]: the task
    /// pilot, step-failure and PR conflict recovery name the complexity pool
    /// they draw from as `workflow.<tier>_complexity_crews`, and draw once per
    /// run, frozen under that key like a final-recovery draw.
    ///
    /// The draw keeps the pool's weights and skips disabled crews, then the
    /// members this run's `allowed_crews` excludes. Crews at one of this
    /// host's provider limits are skipped while any member is left; when every
    /// member is limited the draw stays as it was and its source names the
    /// limit. Crews covered by a host provider failure hold
    /// (`provider_unavailable` or `provider_capacity`) are skipped the same
    /// way, after that limit narrowing. An empty or wholly disabled pool falls
    /// back to `workflow.default_crew`, as an unassigned task does, and the
    /// frozen record's source says so.
    ///
    /// [ORB-15792] An activity that inspects pinned source natively (the task
    /// pilot) draws only crews whose provider has that surface, through
    /// [`Self::inspection_activity_candidates`], and its source names the
    /// filter. Any other activity draws from the whole pool.
    pub(crate) fn complexity_pool_activity_crew(
        &self,
        input: &Value,
        complexity: TaskComplexity,
        random: &mut impl FnMut() -> Result<u64, OrbitError>,
    ) -> Result<Crew, OrbitError> {
        let key = complexity_crews_key(complexity);
        let inspection = requires_native_inspection(input);
        self.frozen_activity_crew(input, &key, &mut |allowlist| {
            let (candidates, source) = if inspection {
                self.inspection_activity_candidates(complexity)?
            } else if let Some(candidates) = self.configured_complexity_pool(complexity)? {
                (candidates, key.clone())
            } else {
                tracing::warn!(
                    pool = %key,
                    "the pool is empty or wholly disabled; this run's activity crew falls back \
                     to workflow.default_crew",
                );
                (
                    vec![sole_candidate(self.resolve_crew_for_task(None, None)?)],
                    format!("workflow.default_crew ({key} is empty)"),
                )
            };
            let candidates = permitted_candidates(candidates, &source, allowlist)?;
            let (candidates, source) = self.provider_unlimited_candidates(candidates, source);
            let (candidates, source) =
                self.apply_host_provider_holds(candidates, source, Utc::now());
            let selected = weighted_draw(&candidates, &source, random)?;
            Ok(activity_crew_draw(selected, &source, &candidates))
        })
    }

    /// The candidates and source of an inspection-requiring activity draw
    /// from `complexity`'s pool, before the run's allowlist and provider
    /// limits apply: the inspection-capable members of the first tier from
    /// `complexity` upward that has one, else an inspection-capable
    /// `workflow.default_crew`.
    fn inspection_activity_candidates(
        &self,
        complexity: TaskComplexity,
    ) -> Result<(Vec<CrewCandidate>, String), OrbitError> {
        let key = complexity_crews_key(complexity);
        let Some(pool) = self.inspection_capable_pool(complexity)? else {
            tracing::warn!(
                pool = %key,
                "no crew in the pool or a higher tier can inspect natively; this run's activity \
                 crew falls back to workflow.default_crew",
            );
            return Ok((
                vec![sole_candidate(
                    self.inspection_capable_default_crew(complexity)?,
                )],
                format!(
                    "workflow.default_crew (native inspection: no crew in {key} or a higher tier \
                     can inspect)"
                ),
            ));
        };
        let mut source = complexity_crews_key(pool.tier);
        if pool.tier != complexity {
            source.push_str(&format!(" (no crew in {key} can inspect)"));
        }
        if pool.skipped.is_empty() {
            source.push_str("; native inspection");
        } else {
            source.push_str(&format!(
                "; native inspection, skipped: {}",
                pool.skipped.join(", ")
            ));
        }
        Ok((pool.candidates, source))
    }

    /// Run `draw` once per run and `key`, freezing its record in the executing
    /// run's state. Dispatch supplies that identity as `job_run_id` when an
    /// explicit `run_id` names an originating failure; otherwise `run_id` is
    /// the executing run. A follower's originating state need not exist here.
    ///
    /// The first dispatch records its draw in the same run-state transaction
    /// that reads it, so two racing dispatches cannot record different crews.
    fn frozen_activity_crew(
        &self,
        input: &Value,
        key: &str,
        draw: &mut dyn FnMut(Option<&CrewAllowlist>) -> Result<ActivityCrewDraw, OrbitError>,
    ) -> Result<Crew, OrbitError> {
        let run_id = input
            .get("job_run_id")
            .or_else(|| input.get("run_id"))
            .and_then(Value::as_str)
            .and_then(non_empty)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "an activity crew drawn from `{key}` needs the input's executing \
                     `job_run_id` or `run_id` to freeze the draw"
                ))
            })?;
        let allowlist = self.crew_allowlist_from_input(input)?;
        let mut drawn: Option<ActivityCrewDraw> = None;
        let update = self
            .stores()
            .jobs()
            .update_run_state(run_id, &mut |_, state| {
                if let Some(frozen) = state.activity_crew_draws.get(key) {
                    drawn = Some(frozen.clone());
                    return Ok(());
                }
                let record = draw(allowlist.as_ref())?;
                state
                    .activity_crew_draws
                    .insert(key.to_string(), record.clone());
                drawn = Some(record);
                Ok(())
            })?;
        if update != RunStateUpdate::Updated {
            return Err(OrbitError::InvalidInput(format!(
                "run '{run_id}' has no persisted state to freeze its `{key}` draw in"
            )));
        }
        let draw = drawn.ok_or_else(|| {
            OrbitError::Execution(format!(
                "`{key}` draw for run '{run_id}' did not run inside its state update"
            ))
        })?;
        self.resolve_crew_for_task(Some(&draw.crew), None)
    }

    /// `candidates` without the crews at one of this host's provider limits,
    /// with `source` naming the skipped crews. When every candidate is
    /// limited, or none is, they are returned as they were.
    fn provider_unlimited_candidates(
        &self,
        candidates: Vec<CrewCandidate>,
        source: String,
    ) -> (Vec<CrewCandidate>, String) {
        let gate = self.provider_limit_gate(chrono::Utc::now());
        let (limited, unlimited): (Vec<_>, Vec<_>) = candidates
            .iter()
            .cloned()
            .partition(|candidate| gate.limit_for(&candidate.crew).is_some());
        if limited.is_empty() {
            return (candidates, source);
        }
        let detail = gate.skipped(limited.iter().map(|candidate| &candidate.crew));
        if unlimited.is_empty() {
            return (
                candidates,
                format!("{source}; provider limit, none skipped: {detail}"),
            );
        }
        (unlimited, format!("{source}; provider limit: {detail}"))
    }

    /// `candidates` without the crews a host provider failure hold covers at
    /// `now`, with `source` naming the hold. When every candidate is held, or
    /// none is, they are returned as they were. The limit narrowing has
    /// already run: a crew dropped there cannot keep a held crew in the draw.
    fn apply_host_provider_holds(
        &self,
        candidates: Vec<CrewCandidate>,
        source: String,
        now: DateTime<Utc>,
    ) -> (Vec<CrewCandidate>, String) {
        let holds = self.standing_host_provider_holds(now);
        if holds.is_empty() {
            return (candidates, source);
        }
        let (held, free): (Vec<_>, Vec<_>) = candidates
            .into_iter()
            .partition(|candidate| holds.iter().any(|hold| hold.covers(&candidate.crew, now)));
        if held.is_empty() {
            return (free, source);
        }
        let detail = provider_hold_detail(&holds, &held, now);
        if free.is_empty() {
            (
                held,
                format!("{source}; provider hold, none skipped: {detail}"),
            )
        } else {
            (free, format!("{source}; provider hold: {detail}"))
        }
    }

    /// Why the task-pilot routine must not start a run at `now`, when every
    /// positive-weight inspection candidate left after the provider-limit
    /// narrowing is under a host hold.
    ///
    /// An unreadable pool is not a hold: the draw reports that configuration
    /// problem itself. A pool that still has a crew the hold does not cover
    /// returns `None`, and the draw skips the held crews.
    pub(crate) fn task_pilot_provider_withhold(&self, now: DateTime<Utc>) -> Option<String> {
        let (candidates, source) = match self.inspection_activity_candidates(TaskComplexity::Low) {
            Ok(drawn) => drawn,
            Err(error) => {
                tracing::warn!("task-pilot provider hold reads no crew pool: {error}");
                return None;
            }
        };
        let candidates: Vec<_> = candidates
            .into_iter()
            .filter(|candidate| candidate.weight > 0)
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let (candidates, source) = self.provider_unlimited_candidates(candidates, source);
        let holds = self.standing_host_provider_holds(now);
        if holds.is_empty()
            || candidates
                .iter()
                .any(|candidate| !holds.iter().any(|hold| hold.covers(&candidate.crew, now)))
        {
            return None;
        }
        let detail = provider_hold_detail(&holds, &candidates, now);
        Some(format!("provider_hold: no crew left in {source}; {detail}"))
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

/// `<hold>[, <hold>...]; crews <list> skipped` for the held `crews`.
fn provider_hold_detail(
    holds: &[HostProviderHold],
    held: &[CrewCandidate],
    now: DateTime<Utc>,
) -> String {
    let mut details = Vec::new();
    for hold in holds {
        if held
            .iter()
            .any(|candidate| hold.covers(&candidate.crew, now))
        {
            let detail = hold.detail();
            if !details.contains(&detail) {
                details.push(detail);
            }
        }
    }
    let mut names = held
        .iter()
        .map(|candidate| candidate.crew.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    format!("{}; crews {} skipped", details.join(", "), names.join(", "))
}
