//! Requeueing a published owner: the requeue source and its refusal, the
//! preservation record, published resume and owner source identity.

use super::super::*;
use super::source_checks::{
    OWN_PUBLICATION_KEYS, adoptable_published_source, footprint_held, parse_preserved,
    preserved_entry, require_stopped_failed_requeue_source,
};
use super::{PRESERVED, PreservedPublication, PublishedResume};
use crate::application::contracts::OwnerSourceIdentityRefusal;
use orbit_types::workflow;

impl OrbitRuntime {
    /// The preserved publication `run` adopted, when `run` is a resumed
    /// published delivery that ended held on its busy deferred footprint
    /// without publishing anything of its own.
    ///
    /// Such a run skips final recovery, so nothing re-preserves the
    /// publication under its id, and terminal cleanup leaves the task bound
    /// to it. It changed nothing remote: the publication it adopted is still
    /// exactly the one the next run must adopt, so the published-resume
    /// lookups judge that source instead [ORB-24993]. The adoption is
    /// authenticated as recorded ([`Self::read_owner_publication_binding`]);
    /// anything unproven answers `None` and keeps the ordinary refusal.
    pub(in crate::application::job::resume::source_repair) fn footprint_held_adopted_source(
        &self,
        run: &JobRun,
    ) -> Result<Option<String>, OrbitError> {
        if !footprint_held(run) {
            return Ok(None);
        }
        let Some(state) = self.read_run_state(&run.run_id)? else {
            return Ok(None);
        };
        if OWN_PUBLICATION_KEYS
            .iter()
            .any(|key| state.pipeline.get(*key).is_some_and(|v| !v.is_null()))
        {
            return Ok(None);
        }
        Ok(self
            .read_owner_publication_binding(&run.run_id, false)
            .ok()
            .flatten()
            .map(|binding| binding.source_run_id))
    }

    /// `run_id`, or the publication it adopted when it is a footprint-held
    /// resumed delivery ([`Self::footprint_held_adopted_source`]).
    pub(super) fn published_source_through_footprint_hold(
        &self,
        run_id: &str,
    ) -> Result<String, OrbitError> {
        let adopted = match self.get_job_run_backend(run_id)? {
            Some(run) => self.footprint_held_adopted_source(&run)?,
            None => None,
        };
        Ok(adopted.unwrap_or_else(|| run_id.to_string()))
    }

    /// `run_id`, when a requeue of `task` would preserve its publication: a
    /// local, unclaimed owner PR run that pushed or opened its pull request.
    fn published_owner_requeue_source(
        &self,
        task: &Task,
        run_id: &str,
    ) -> Result<Option<JobRun>, OrbitError> {
        if !self.task_run_is_local(task) {
            return Ok(None);
        }
        let Some(source) = self.get_job_run_backend(run_id)? else {
            return Ok(None);
        };
        let Some(state) = self.read_run_state(run_id)? else {
            return Ok(None);
        };
        if source.job_id != workflow::TASK_PR_PIPELINE_JOB
            || source.input.as_ref().is_some_and(|i| i["claimed"] == true)
            || !["push", "pr_open"]
                .iter()
                .any(|key| state.pipeline.get(key).is_some_and(|v| !v.is_null()))
        {
            return Ok(None);
        }
        Ok(Some(source))
    }

    /// Why a requeue of `task` must not preserve `run_id`'s publication: the
    /// run published, but is not the exact owner-local single-task execution
    /// the next run's candidate resume authenticates, so a successor would
    /// only fail there. `None` when nothing is published or the source passes
    /// that identity check.
    pub(crate) fn published_owner_requeue_refusal(
        &self,
        task: &Task,
        run_id: &str,
    ) -> Result<Option<OwnerSourceIdentityRefusal>, OrbitError> {
        match self.published_owner_requeue_source(task, run_id)? {
            Some(source) => self.owner_source_identity_refusal(&source, task),
            None => Ok(None),
        }
    }

    /// Called under the final-recovery task lock; stored atomically with its
    /// decision. Refuses a publication candidate resume would refuse
    /// ([`Self::published_owner_requeue_refusal`]) rather than preserve it.
    pub(crate) fn published_owner_requeue_record(
        &self,
        task: &Task,
        run_id: &str,
    ) -> Result<Option<TaskHistoryEntry>, OrbitError> {
        let Some(source) = self.published_owner_requeue_source(task, run_id)? else {
            return Ok(None);
        };
        self.owner_source_identity(&source, task)?;
        let record = PreservedPublication {
            schema_version: 1,
            source_run_id: run_id.into(),
            task_digest: task_digest(task)?,
            failure_handoff: false,
        };
        preserved_entry(&record).map(Some)
    }

    /// Whether `task`'s next run resumes the publication a final-recovery
    /// requeue of its current run preserved, judged from durable records
    /// alone: no Git or forge read, so admission can ask it of every backlog
    /// task on every pass.
    ///
    /// Such a run does not edit the working tree before it adopts the
    /// existing branch and pull request, so admission does not hold it behind
    /// other tasks' context locks; any agent edit it makes later takes the
    /// footprint locks first (`acquire_deferred_footprint`), unless an
    /// operator's explicit ship un-defers them at the gate. The full
    /// authentication — branch, remote and pull-request heads — still runs
    /// when the next run adopts, and refuses what this cannot see.
    pub(crate) fn published_resume(
        &self,
        task: &Task,
    ) -> Result<Option<PublishedResume>, OrbitError> {
        let Some(bound_id) = task.job_run_id.as_deref() else {
            return Ok(None);
        };
        if task.status != TaskStatus::Backlog || !self.task_run_is_local(task) {
            return Ok(None);
        }
        // A footprint hold requeues the task still bound to the held run,
        // which only adopted this publication [ORB-24993].
        let source_id = self.published_source_through_footprint_hold(bound_id)?;
        let source_id = source_id.as_str();
        let Some(source) = self.get_job_run_backend(source_id)? else {
            return Ok(None);
        };
        if source.job_id != workflow::TASK_PR_PIPELINE_JOB
            || source.input.as_ref().is_none_or(|i| {
                i["claimed"] == true || carried_task_ids(i) != BTreeSet::from([task.id.clone()])
            })
            || !adoptable_published_source(&source)
        {
            return Ok(None);
        }
        let entries = self.get_task_history(&task.id)?;
        let mut records = entries
            .iter()
            .filter(|e| e.event == PRESERVED)
            .filter_map(|e| {
                (e.by == "system")
                    .then(|| parse_preserved(e).ok())
                    .flatten()
                    .filter(|record| record.source_run_id == source_id)
                    .map(|record| (e.at, record))
            });
        let (Some((preserved_at, record)), None) = (records.next(), records.next()) else {
            return Ok(None);
        };
        // The record digests the task as stored; a caller's copy may carry
        // admission's resolved crew. Admission's later system crew redraws
        // leave the preserved identity intact [ORB-18560].
        if record.schema_version != 1
            || record.failure_handoff
            || !super::super::adoption::task_matches_before_crew_redraws(
                &self.get_task(&task.id)?,
                &entries,
                preserved_at,
                &record.task_digest,
            )?
            || entries.iter().any(|e| {
                e.event == orbit_types::task::CANDIDATE_DISCARDED_EVENT && e.at >= source.created_at
            })
            || self.recorded_final_recovery(&task.id, source_id)?
                != Some(crate::application::task::FinalRecoveryOutcome::Requeued)
        {
            return Ok(None);
        }
        let pull_request = self.read_run_state(source_id)?.and_then(|state| {
            state.pipeline.get("pr_open")?["pr_number"]
                .as_str()
                .map(str::to_string)
        });
        Ok(Some(PublishedResume {
            source_run_id: source_id.to_string(),
            pull_request,
        }))
    }

    /// Authenticate the `[BLOCKED]` PR the failure handoff of `task`'s
    /// current run published, exactly as the next run's adoption will, and
    /// return the preservation record that lets that run resume it, with the
    /// binding it resumes. Called by a requeue of the blocked task before it
    /// takes the task write lock, since authenticating asks the remote and
    /// the forge; nothing is written here.
    pub(crate) fn failure_handoff_requeue_record(
        &self,
        task: &Task,
    ) -> Result<(TaskHistoryEntry, SourceRepairBinding), OrbitError> {
        let (source_id, source) = self.stopped_failed_requeue_source(
            task,
            "the task's failure handoff belongs to another machine",
            "the task has no run whose failure handoff it could resume",
        )?;
        let entries = self.get_task_history(&task.id)?;
        require_stopped_failed_requeue_source(&source)?;
        self.require_current_requeue_source(task, &source, &source_id, &entries)?;
        let state = self
            .read_run_state(&source_id)?
            .ok_or_else(|| refuse("source checkpoints missing"))?;
        if state.failure_activity_checkpoint.is_none() {
            return Err(refuse("the run published no failure-handoff PR"));
        }
        let definition = self.source_repair_definition(&source)?;
        let (_, _, binding) =
            self.observe_handoff_publication(source, task.clone(), state, definition)?;
        let record = PreservedPublication {
            schema_version: 1,
            source_run_id: source_id,
            task_digest: task_digest(task)?,
            failure_handoff: true,
        };
        Ok((preserved_entry(&record)?, binding))
    }

    /// The preservation record an operator requeue appends for `entry`, given
    /// the records `task` already holds. Admission takes exactly one record
    /// per source run, so the same record already on record binds the
    /// publication and nothing is appended (`None`). A different record for
    /// that run would be a second one, which admission refuses, so it is
    /// refused here before anything is written. Read the whole preservation
    /// history: an identical first record cannot hide a duplicate, malformed
    /// or untrusted record that the next admission would refuse.
    pub(crate) fn preservation_to_append(
        &self,
        task: &Task,
        entry: TaskHistoryEntry,
    ) -> Result<Option<TaskHistoryEntry>, OrbitError> {
        let record = parse_preserved(&entry)?;
        let mut matching = None;
        for existing in self
            .get_task_history(&task.id)?
            .iter()
            .filter(|e| e.event == PRESERVED)
        {
            if existing.by != "system" {
                return Err(refuse("untrusted publication preservation"));
            }
            let existing = parse_preserved(existing)?;
            if existing.source_run_id != record.source_run_id {
                continue;
            }
            if matching.replace(existing).is_some() {
                return Err(refuse("published candidate lacks a unique requeue binding"));
            }
        }
        match matching {
            Some(existing) if existing == record => Ok(None),
            Some(_) => Err(refuse(format!(
                "a different publication of run `{}` is already preserved on the task",
                record.source_run_id
            ))),
            None => Ok(Some(entry)),
        }
    }

    /// Whether `task`, last delivered by an owner PR run, has a pull request
    /// or a publication-preservation record on record. With no
    /// failure-handoff receipt binding them, the next admission refuses such
    /// a task (`owner_published_candidate`), so a requeue without a discard
    /// would only strand it. A claimed or local-only delivery keeps its own
    /// candidate contract, as that admission does: a claim's kept candidate
    /// republishes through its own pull request.
    pub(crate) fn unbound_publication_on_record(&self, task: &Task) -> Result<bool, OrbitError> {
        if !self.task_run_is_local(task) {
            return Ok(false);
        }
        if let Some(run_id) = task.job_run_id.as_deref() {
            let owner_pr = self.get_job_run_backend(run_id)?.is_some_and(|run| {
                run.job_id == workflow::TASK_PR_PIPELINE_JOB
                    && run
                        .input
                        .as_ref()
                        .is_none_or(|input| input["claimed"] != true)
            });
            // A run missing here is a follower's claimed leaf.
            if !owner_pr {
                return Ok(false);
            }
        }
        Ok(task.github_pr_number().is_some()
            || self
                .get_task_history(&task.id)?
                .iter()
                .any(|entry| entry.event == PRESERVED))
    }

    /// Resolve `task`'s bound run, using the caller's nonlocal/missing messages.
    pub(in crate::application::job::resume::source_repair) fn stopped_failed_requeue_source(
        &self,
        task: &Task,
        nonlocal: &'static str,
        missing_binding: &'static str,
    ) -> Result<(String, JobRun), OrbitError> {
        if !self.task_run_is_local(task) {
            return Err(refuse(nonlocal));
        }
        let source_id = task
            .job_run_id
            .as_deref()
            .ok_or_else(|| refuse(missing_binding))?;
        let source = self
            .get_job_run_backend(source_id)?
            .ok_or_else(|| refuse("the task's run is missing"))?;
        self.owner_source_identity(&source, task)?;
        Ok((source_id.to_string(), source))
    }

    /// Refuse on a discard at/after `source` creation or a newer run.
    /// `entries` is already-read history, preserving each path's read timing.
    pub(in crate::application::job::resume::source_repair) fn require_current_requeue_source(
        &self,
        task: &Task,
        source: &JobRun,
        source_id: &str,
        entries: &[TaskHistoryEntry],
    ) -> Result<(), OrbitError> {
        if entries.iter().any(|e| {
            e.event == orbit_types::task::CANDIDATE_DISCARDED_EVENT && e.at >= source.created_at
        }) {
            return Err(refuse("an operator discarded this run's candidate"));
        }
        let newest = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            job_id: Some(workflow::TASK_PR_PIPELINE_JOB.into()),
            task_id: Some(task.id.clone()),
            limit: Some(1),
            include_steps: false,
            ..Default::default()
        })?;
        if newest.first().is_none_or(|run| run.run_id != source_id) {
            if let Some(run) = newest.first().filter(|run| {
                run.state == JobRunState::Held
                    && run.retry_source_run_id.as_deref() == Some(source_id)
            }) {
                let held = self.get_job_run_backend(&run.run_id)?;
                if held
                    .as_ref()
                    .and_then(|run| run.steps.last())
                    .and_then(|step| step.error_code.as_deref())
                    == Some(workflow::DeliveryRefusal::CheckEvidenceInconclusive.error_code())
                {
                    return Err(refuse(format!(
                        "newer execution makes publication association ambiguous; completion run '{}' is held on inconclusive check evidence; resume it with `orbit job resume {}`",
                        run.run_id, run.run_id
                    )));
                }
            }
            return Err(refuse(
                "newer execution makes publication association ambiguous",
            ));
        }
        Ok(())
    }

    /// Refuse unless `source` is an exact owner-local single-task execution
    /// of `task`, naming the predicate that failed
    /// ([`Self::owner_source_identity_refusal`]).
    pub(in crate::application::job::resume::source_repair) fn owner_source_identity(
        &self,
        source: &JobRun,
        task: &Task,
    ) -> Result<(), OrbitError> {
        match self.owner_source_identity_refusal(source, task)? {
            None => Ok(()),
            Some(OwnerSourceIdentityRefusal::LocalIdentityMissing) => {
                Err(refuse("local identity missing"))
            }
            Some(reason) => Err(refuse(format!(
                "published candidate is not an exact owner-local single-task execution: {reason}"
            ))),
        }
    }

    /// The first owner-source identity predicate `source` fails for `task`,
    /// or `None` when it is an exact owner-local single-task execution. The
    /// predicates run in a fixed order and the store reads only after the
    /// record checks pass, so each failing source reports one reason.
    pub(crate) fn owner_source_identity_refusal(
        &self,
        source: &JobRun,
        task: &Task,
    ) -> Result<Option<OwnerSourceIdentityRefusal>, OrbitError> {
        let Some(local) = self.automation_machine_identity() else {
            return Ok(Some(OwnerSourceIdentityRefusal::LocalIdentityMissing));
        };
        let reason = if source.job_id != workflow::TASK_PR_PIPELINE_JOB {
            OwnerSourceIdentityRefusal::JobKind
        } else if source
            .executed_on
            .as_ref()
            .is_none_or(|o| o.machine_id != local)
        {
            OwnerSourceIdentityRefusal::SourceMachine
        } else if task
            .job_run_machine
            .as_ref()
            .is_none_or(|o| o.machine_id != local)
        {
            OwnerSourceIdentityRefusal::TaskMachine
        } else if source.input.as_ref().is_some_and(|i| i["claimed"] == true) {
            OwnerSourceIdentityRefusal::ClaimedInput
        } else if source
            .input
            .as_ref()
            .is_none_or(|i| carried_task_ids(i) != BTreeSet::from([task.id.clone()]))
        {
            OwnerSourceIdentityRefusal::TaskSet
        } else if self
            .stores()
            .jobs()
            .local_pull_for_run(&source.run_id)?
            .is_some()
        {
            OwnerSourceIdentityRefusal::LocalPull
        } else if !self.resolve_task_execution_claims(&task.id)?.is_empty() {
            OwnerSourceIdentityRefusal::ExecutionClaim
        } else {
            return Ok(None);
        };
        Ok(Some(reason))
    }
}
