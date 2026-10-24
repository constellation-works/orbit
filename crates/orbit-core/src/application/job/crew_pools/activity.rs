use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_config::canonical_crew_pool;
use orbit_types::identity::Crew;
use orbit_types::task::TaskComplexity;
use orbit_types::workflow::{
    ActivityCrewDraw, CrewPurpose, FINAL_RECOVERY_CREWS_CONFIG_KEY, FINAL_RECOVERY_CREWS_KEY,
    RunStateUpdate,
};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::distributed::HostProviderHold;
use crate::runtime::engine::crew::CrewAllowlist;
use crate::runtime::run_input::non_empty;

use super::draw::{complexity_crews_key, permitted_candidates, sole_candidate};
use super::purpose::activity_fallback_tier;
use super::types::CrewCandidate;

impl OrbitRuntime {
    /// The crew a `final_recovery` activity runs as: one draw from
    /// `workflow.purpose_crews.final_recovery`, frozen in the executing run's
    /// state under the activity binding [`FINAL_RECOVERY_CREWS_KEY`], the key
    /// earlier runs froze theirs under.
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
        let source = FINAL_RECOVERY_CREWS_CONFIG_KEY;
        self.frozen_activity_crew(input, FINAL_RECOVERY_CREWS_KEY, &mut |allowlist| {
            let candidates = self.final_recovery_candidates()?;
            let candidates = permitted_candidates(candidates, source, allowlist)?;
            Ok(self
                .draw_crew_pool(candidates, source, random)?
                .into_activity_draw())
        })
    }

    /// The crew system work without a task runs as [ORB-15663]: an activity
    /// naming `workflow.<tier>_complexity_crews` as its `crew_config_key`, or a
    /// purpose activity whose own pool is unset
    /// ([`Self::purpose_activity_crew`]), draws once per run, frozen under the
    /// key it names like a final-recovery draw.
    ///
    /// The draw keeps the pool's weights and skips disabled crews, then the
    /// members this run's `allowed_crews` excludes. Crews at one of this
    /// host's provider limits are skipped while any member is left; when every
    /// member is limited the draw stays as it was and its source names the
    /// limit. Crews covered by a host provider failure hold
    /// (`provider_unavailable` or `provider_capacity`) are skipped the same
    /// way, after that limit narrowing. An empty or wholly disabled pool falls
    /// back to `workflow.default_crew`, as an unassigned task does, and the
    /// frozen record's source says so. Any configured crew may be drawn,
    /// whatever its provider: an inspection activity's trust comes from the
    /// observed-revision proof its apply step checks.
    pub(crate) fn complexity_pool_activity_crew(
        &self,
        input: &Value,
        complexity: TaskComplexity,
        random: &mut impl FnMut() -> Result<u64, OrbitError>,
    ) -> Result<Crew, OrbitError> {
        let key = complexity_crews_key(complexity);
        let now = Utc::now();
        // The hold scans read run history. They run before the run-state
        // write transaction, and not at all once this run has frozen its draw.
        let holds = if self.frozen_activity_draw(input, &key)?.is_some() {
            Vec::new()
        } else {
            self.standing_host_provider_holds(now)
        };
        self.frozen_activity_crew(input, &key, &mut |allowlist| {
            let (candidates, source) = self.activity_draw_candidates(complexity, allowlist, now)?;
            let (candidates, source) = skip_held_candidates(candidates, source, &holds, now);
            Ok(self
                .draw_crew_pool(candidates, &source, random)?
                .into_activity_draw())
        })
    }

    /// The crew an activity whose `crew_config_key` names `purpose`'s
    /// `workflow.purpose_crews` key runs as: one draw per run, frozen under
    /// that key.
    ///
    /// The draw is over the purpose's pool when one is configured, narrowed
    /// the way a complexity-pool draw is: disabled and zero-weight crews, then
    /// the members this run's `allowed_crews` excludes, then crews at a
    /// provider limit or under a host provider hold while any member is left.
    /// A purpose with no such pool draws exactly as its activities
    /// did before purposes existed, from the complexity pool
    /// [`activity_fallback_tier`] names, and the frozen record's source names
    /// that pool. A run that froze its draw under that complexity key before
    /// the purpose existed keeps that crew on resume.
    pub(crate) fn purpose_activity_crew(
        &self,
        input: &Value,
        purpose: CrewPurpose,
        random: &mut impl FnMut() -> Result<u64, OrbitError>,
    ) -> Result<Crew, OrbitError> {
        let key = purpose.config_key();
        let tier = activity_fallback_tier(purpose).ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "`{key}` names a purpose no activity draws its crew from"
            ))
        })?;
        let frozen = self.frozen_activity_draw(input, key)?.is_some();
        if !frozen
            && self
                .frozen_activity_draw(input, &complexity_crews_key(tier))?
                .is_some()
        {
            return self.complexity_pool_activity_crew(input, tier, random);
        }
        let now = Utc::now();
        let holds = if frozen {
            Vec::new()
        } else {
            self.standing_host_provider_holds(now)
        };
        self.frozen_activity_crew(input, key, &mut |allowlist| {
            let (candidates, source) =
                self.purpose_draw_candidates(purpose, tier, allowlist, now)?;
            let (candidates, source) = skip_held_candidates(candidates, source, &holds, now);
            Ok(self
                .draw_crew_pool(candidates, &source, random)?
                .into_activity_draw())
        })
    }

    /// What a purpose activity draw chooses from before host provider holds
    /// apply: the purpose's pool narrowed like
    /// [`Self::activity_draw_candidates`], or that complexity draw itself for
    /// `tier` when the purpose has no usable pool.
    fn purpose_draw_candidates(
        &self,
        purpose: CrewPurpose,
        tier: TaskComplexity,
        allowlist: Option<&CrewAllowlist>,
        now: DateTime<Utc>,
    ) -> Result<(Vec<CrewCandidate>, String), OrbitError> {
        let Some((candidates, source)) = self.purpose_pool_candidates(purpose)? else {
            return self.activity_draw_candidates(tier, allowlist, now);
        };
        let candidates = permitted_candidates(candidates, &source, allowlist)?;
        Ok(self.provider_unlimited_candidates(candidates, source, now))
    }

    /// What a complexity-pool activity draw chooses from before host provider
    /// holds apply: the pool's members, falling back as
    /// [`Self::complexity_pool_activity_crew`] describes, without disabled,
    /// zero-weight or allowlist-excluded crews, then without crews at one of
    /// this host's provider limits while any member is left. The draw and the
    /// task-pilot routine's withhold both read it, so one narrowing rule
    /// serves both.
    fn activity_draw_candidates(
        &self,
        complexity: TaskComplexity,
        allowlist: Option<&CrewAllowlist>,
        now: DateTime<Utc>,
    ) -> Result<(Vec<CrewCandidate>, String), OrbitError> {
        let key = complexity_crews_key(complexity);
        let (candidates, source) =
            if let Some(candidates) = self.configured_complexity_pool(complexity)? {
                (candidates, key)
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
        Ok(self.provider_unlimited_candidates(candidates, source, now))
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
        let run_id = activity_draw_run_id(input, key)?;
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

    /// The draw this run already froze under `key`, read outside any write
    /// transaction. The frozen-draw transaction stays authoritative: a draw
    /// that lands between this read and it still wins there.
    fn frozen_activity_draw(
        &self,
        input: &Value,
        key: &str,
    ) -> Result<Option<ActivityCrewDraw>, OrbitError> {
        let run_id = activity_draw_run_id(input, key)?;
        Ok(self
            .read_run_state(run_id)?
            .and_then(|mut state| state.activity_crew_draws.remove(key)))
    }

    /// `candidates` without the crews at one of this host's provider limits,
    /// with `source` naming the skipped crews. When every candidate is
    /// limited, or none is, they are returned as they were.
    fn provider_unlimited_candidates(
        &self,
        candidates: Vec<CrewCandidate>,
        source: String,
        now: DateTime<Utc>,
    ) -> (Vec<CrewCandidate>, String) {
        let gate = self.provider_limit_gate(now);
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

    /// Why the task-pilot routine must not start a run at `now`: every crew
    /// its pilots' draw could choose ([`Self::purpose_draw_candidates`] for
    /// the task-pilot purpose) is under a host hold.
    ///
    /// The routine submits no `allowed_crews`, so its runs draw with no
    /// allowlist. An unreadable pool is not a hold: the draw reports that
    /// configuration problem itself. A pool that still has a crew the hold
    /// does not cover returns `None`, and the draw skips the held crews.
    pub(crate) fn task_pilot_provider_withhold(&self, now: DateTime<Utc>) -> Option<String> {
        let holds = self.standing_host_provider_holds(now);
        if holds.is_empty() {
            return None;
        }
        let (candidates, source) = match self.purpose_draw_candidates(
            CrewPurpose::TaskPilot,
            TaskComplexity::Low,
            None,
            now,
        ) {
            Ok(drawn) => drawn,
            Err(error) => {
                tracing::warn!("task-pilot provider hold reads no crew pool: {error}");
                return None;
            }
        };
        if candidates.is_empty()
            || candidates
                .iter()
                .any(|candidate| !holds.iter().any(|hold| hold.covers(&candidate.crew, now)))
        {
            return None;
        }
        let detail = provider_hold_detail(&holds, &candidates, now);
        Some(format!("provider_hold: no crew left in {source}; {detail}"))
    }

    /// The enabled members of `workflow.purpose_crews.final_recovery`, each with its
    /// tickets. An empty pool disables final recovery, and a pool whose every
    /// member is disabled is refused the same way rather than falling back to
    /// another crew.
    fn final_recovery_candidates(&self) -> Result<Vec<CrewCandidate>, OrbitError> {
        let source = FINAL_RECOVERY_CREWS_CONFIG_KEY;
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

/// The executing run a frozen activity draw belongs to. Dispatch supplies it
/// as `job_run_id` when an explicit `run_id` names an originating failure.
fn activity_draw_run_id<'a>(input: &'a Value, key: &str) -> Result<&'a str, OrbitError> {
    input
        .get("job_run_id")
        .or_else(|| input.get("run_id"))
        .and_then(Value::as_str)
        .and_then(non_empty)
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "an activity crew drawn from `{key}` needs the input's executing \
                 `job_run_id` or `run_id` to freeze the draw"
            ))
        })
}

/// `candidates` without the crews `holds` cover at `now`, with `source`
/// naming the hold. When every candidate is held, or none is, they are
/// returned as they were. The limit narrowing has already run: a crew dropped
/// there cannot keep a held crew in the draw.
fn skip_held_candidates(
    candidates: Vec<CrewCandidate>,
    source: String,
    holds: &[HostProviderHold],
    now: DateTime<Utc>,
) -> (Vec<CrewCandidate>, String) {
    if holds.is_empty() {
        return (candidates, source);
    }
    let (held, free): (Vec<_>, Vec<_>) = candidates
        .into_iter()
        .partition(|candidate| holds.iter().any(|hold| hold.covers(&candidate.crew, now)));
    if held.is_empty() {
        return (free, source);
    }
    let detail = provider_hold_detail(holds, &held, now);
    if free.is_empty() {
        (
            held,
            format!("{source}; provider hold, none skipped: {detail}"),
        )
    } else {
        (free, format!("{source}; provider hold: {detail}"))
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
