//! Crew resolution around claims: fresh owner claim routing over historical
//! experiment crew assignments, and the crew a run's input resolves to at
//! start, where a claimed leaf's automatic assignment is re-drawn for this
//! follower.
use std::path::Path;

use super::draw::weighted_draw;
use crate::OrbitRuntime;
use crate::application::job::JobExperimentPort;
use crate::runtime::engine::crew::{
    BundleCrews, ClaimedTaskSnapshot, TASK_PILOT_JOB, claimed_task_from_input, loaded_job_name,
    mixed_bundle_crew_error, normalized_task_crew, run_input_crew_override,
    task_ids_for_crew_resolution,
};
use orbit_common::OrbitError;
use orbit_types::identity::Crew;
use orbit_types::task::{CrewSource, Task, is_valid_orb_task_id};
use serde_json::Value;

impl OrbitRuntime {
    /// Resolve a fresh claim through the same candidates as local admission.
    /// This transient view is screened and frozen by the owner, so a stale
    /// delivery write-back cannot become a follower's new standing pin.
    pub(crate) fn fresh_claim_crew_task(
        &self,
        task: &Task,
        request: &orbit_store::contract::AdmissionRequest,
    ) -> Result<Task, OrbitError> {
        // Preserve unrelated routing, including human work and standing
        // purpose/type assignments. Only historical crew enrollment needs
        // the fresh-authority projection.
        let capture =
            crate::app::port::JobExperiment(self).job_existing_experiment_capture(&task.id)?;
        if self.is_human_crew(task.crew.as_deref())
            || !capture.assignments.iter().any(|entry| {
                serde_json::from_str::<Value>(&entry.overlay_json)
                    .ok()
                    .is_some_and(|overlay| !overlay["crew"].is_null())
            })
        {
            return Ok(task.clone());
        }
        let pools = self.auto_crew_pools_for_input(&Value::Null)?;
        // The claim's executor owns its provider context.
        let (candidates, source, _) =
            self.auto_task_crew_candidates_before_model_access(task, &pools, None, None)?;
        let runnable = candidates
            .iter()
            .filter(|candidate| {
                request.crews.as_ref().is_none_or(|crews| {
                    crews
                        .unrunnable_reason(Some(&candidate.crew.name))
                        .is_none()
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        // When no member can run, retain the effective pool's candidate so
        // the store reports crew_unavailable instead of silently widening it.
        let candidates = if runnable.is_empty() {
            candidates
        } else {
            runnable
        };
        let mut ticket =
            super::draw::seeded_crew_ticket(format!("claim:{}:{}", request.request_id, task.id));
        let selected = weighted_draw(&candidates, &source, &mut ticket)?;
        let mut view = task.clone();
        view.crew = Some(selected.crew.name.clone());
        if source != "task.crew" {
            view.crew_source = Some(CrewSource::parse(&source));
        }
        Ok(view)
    }
}

impl OrbitRuntime {
    /// The crew a run's input resolves to: its explicit `crew`, else the
    /// crew its tasks agree on.
    pub fn resolve_crew_for_run_input(
        &self,
        input: &Value,
        bundle: BundleCrews,
    ) -> Result<Crew, OrbitError> {
        let cli_override = run_input_crew_override(input);
        let claimed = claimed_task_from_input(input)?;
        let task_crew = self.task_crew_from_run_input(input, claimed.as_ref(), bundle)?;
        self.resolve_crew_for_task(cli_override, task_crew.as_deref())
            .map_err(|error| {
                // A crew the owner chose but this host cannot run fails the
                // leaf by name. Falling back to `default_crew` would ship the
                // owner's task on a provider nobody selected for it.
                match claimed.as_ref().zip(task_crew.as_deref()) {
                    Some((snapshot, crew))
                        if cli_override.is_none() && snapshot.crew() == Some(crew) =>
                    {
                        OrbitError::InvalidInput(format!(
                            "claimed task {} has crew `{crew}` on its owner, which this host \
                             cannot run: {error}; configure and enable `{crew}` here or reassign \
                             the task on the owner (a claimed leaf never falls back to \
                             workflow.default_crew)",
                            snapshot.id,
                        ))
                    }
                    _ => error,
                }
            })
    }

    /// Resolve a run's crew at start, persisting it only when the job at
    /// `yaml_path` can dispatch an agent [ORB-13016].
    ///
    /// Every job still resolves, so a crew misconfiguration (an unknown name,
    /// no crew selected) fails the run at start as it always has. A job made
    /// only of deterministic activities records no crew: no model does its
    /// work, and a crew drawn for it would read as an LLM that never ran. For
    /// the same reason a disabled crew does not refuse it: on a host with no
    /// enabled default crew, retention and reap jobs must still run.
    ///
    /// `bundle` is the caller's authenticated answer to whether this run may
    /// name tasks whose crews differ; see [`BundleCrews`].
    ///
    /// A task-pilot job does not take a human task crew as its agent crew.
    /// Prepare excludes those tasks, and the run keeps an agent crew instead
    /// of failing at start. An explicit human crew on the run input is still
    /// refused, and every other job still refuses a human task crew.
    pub fn record_run_crew_for_job(
        &self,
        run_id: &str,
        input: &Value,
        yaml_path: &Path,
        bundle: BundleCrews,
    ) -> Result<(), OrbitError> {
        let dispatches_agent = self.job_definition_dispatches_agent(yaml_path);
        if dispatches_agent && loaded_job_name(yaml_path).as_deref() == Some(TASK_PILOT_JOB) {
            self.record_task_pilot_run_crew(run_id, input, bundle)?;
        } else if dispatches_agent {
            self.record_run_crew_from_input(run_id, input, bundle)?;
        } else {
            let claimed = claimed_task_from_input(input)?;
            let task_crew = self.task_crew_from_run_input(input, claimed.as_ref(), bundle)?;
            self.lookup_crew_for_task(run_input_crew_override(input), task_crew.as_deref())?;
        }
        Ok(())
    }

    /// The task-pilot run's agent crew. A human task crew is operator work:
    /// it is not this run's crew, and it does not fail the run. Prepare
    /// excludes those tasks. Any other agreed crew resolves as it does for
    /// every job, including the claimed-leaf error.
    fn record_task_pilot_run_crew(
        &self,
        run_id: &str,
        input: &Value,
        bundle: BundleCrews,
    ) -> Result<Crew, OrbitError> {
        let claimed = claimed_task_from_input(input)?;
        let assigned = self.task_crew_from_run_input(input, claimed.as_ref(), bundle)?;
        let crew = if self.is_human_crew(assigned.as_deref()) {
            let task_crew = self.agent_task_crew_from_run_input(input, claimed.as_ref(), bundle)?;
            self.resolve_crew_for_task(run_input_crew_override(input), task_crew.as_deref())?
        } else {
            self.resolve_crew_for_run_input(input, bundle)?
        };
        tracing::info!(
            run_id,
            resolved_crew = %crew.name,
            crew_model = %crew.assignment.model,
            "crew resolved for run",
        );
        self.stores().jobs().record_job_run_crew(run_id, &crew)?;
        Ok(crew)
    }

    fn record_run_crew_from_input(
        &self,
        run_id: &str,
        input: &Value,
        bundle: BundleCrews,
    ) -> Result<Crew, OrbitError> {
        let crew = self.resolve_crew_for_run_input(input, bundle)?;
        tracing::info!(
            run_id,
            resolved_crew = %crew.name,
            crew_model = %crew.assignment.model,
            "crew resolved for run",
        );
        self.stores().jobs().record_job_run_crew(run_id, &crew)?;
        Ok(crew)
    }

    /// Unanimous `task.crew` across every task named by the run/activity
    /// input. A task the input carries a `claimed_task` snapshot for reads its
    /// crew from that snapshot: a pulled task lives in the owner's store, and a
    /// same-id local record, if any, is not it. Other ids read the local store;
    /// missing fixture ids are skipped so fake implementer ids do not fail
    /// resolution. Distinct crews — including a mix of set and unset — fail
    /// closed instead of inheriting `workflow.default_crew`. Authenticated
    /// preparation, drain-pilot, and auto-dispatch runs are exceptions: each
    /// keeps the first task's crew, then partitions tasks before agent work.
    fn task_crew_from_run_input(
        &self,
        input: &Value,
        claimed: Option<&ClaimedTaskSnapshot>,
        bundle: BundleCrews,
    ) -> Result<Option<String>, OrbitError> {
        self.agreed_task_crew(input, claimed, bundle, false)
    }

    /// [`Self::task_crew_from_run_input`], ignoring human task crews.
    ///
    /// A task pilot uses this only after the unfiltered agreement is itself a
    /// human crew. Human crews then neither become the run's agent crew nor
    /// count as a disagreement, so an all-human bundle resolves with no task
    /// crew and a partition whose first crew is human keeps the first agent
    /// crew. A mixed bundle that already failed closed stays failed: this
    /// walk does not replace that result.
    fn agent_task_crew_from_run_input(
        &self,
        input: &Value,
        claimed: Option<&ClaimedTaskSnapshot>,
        bundle: BundleCrews,
    ) -> Result<Option<String>, OrbitError> {
        self.agreed_task_crew(input, claimed, bundle, true)
    }

    fn agreed_task_crew(
        &self,
        input: &Value,
        claimed: Option<&ClaimedTaskSnapshot>,
        bundle: BundleCrews,
        skip_human_task_crews: bool,
    ) -> Result<Option<String>, OrbitError> {
        let mut agreed: Option<Option<String>> = None;
        for task_id in task_ids_for_crew_resolution(input) {
            if !is_valid_orb_task_id(&task_id) {
                continue;
            }
            let crew = match claimed.filter(|snapshot| snapshot.id == task_id) {
                // [ORB-17773] An automatic assignment is re-drawn under
                // this follower's own usage pressure, and [ORB-25663] around
                // a Muse model this follower was verified to be refused;
                // `None` keeps the owner's crew exactly as the snapshot
                // carries it. A refusal that leaves no crew is a typed hold.
                Some(snapshot) => self
                    .claimed_leaf_redrawn_crew(
                        input,
                        &snapshot.id,
                        snapshot.crew(),
                        snapshot.crew_source.as_ref(),
                        snapshot.complexity,
                    )?
                    .or_else(|| snapshot.crew().map(ToOwned::to_owned)),
                None => match self.stores().tasks().get_task(&task_id)? {
                    Some(task) => normalized_task_crew(task.crew.as_deref()),
                    None => continue,
                },
            };
            // A human crew is operator work. When this walk is choosing a
            // task pilot's agent crew, that assignment is not a vote.
            if skip_human_task_crews && self.is_human_crew(crew.as_deref()) {
                continue;
            }
            match &agreed {
                None => agreed = Some(crew),
                Some(existing) if existing == &crew => {}
                // The live preparation attempt's run keeps the first task's
                // crew while prepare splits its claim by crew before pilot
                // workers run. A verified drain pilot similarly partitions by
                // live crew. The auto-dispatch run also keeps the first task's
                // crew; its backlog action emits singleton gate
                // bundles before any task worker starts. Every other bundle,
                // including one whose input merely carries a claim, fails
                // closed below [ORB-16860].
                Some(existing)
                    if matches!(
                        bundle,
                        BundleCrews::PreparationPartitions
                            | BundleCrews::AutoDispatchSingletons
                            | BundleCrews::DrainPartitions
                    ) =>
                {
                    return Ok(existing.clone());
                }
                Some(existing) => {
                    return Err(OrbitError::InvalidInput(mixed_bundle_crew_error(
                        existing.as_deref(),
                        crew.as_deref(),
                    )));
                }
            }
        }
        Ok(agreed.flatten())
    }
}
