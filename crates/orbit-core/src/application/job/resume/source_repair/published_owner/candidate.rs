//! The owner's published candidate and its authenticated publication
//! binding.

use super::super::admission::hold_operator_source_repair_refusal;
use super::super::*;
use super::source_checks::{adoptable_published_source, parse_preserved};
use super::{PRESERVED, PreservedPublication};
use orbit_types::workflow;

impl OrbitRuntime {
    pub(crate) fn owner_published_candidate(
        &self,
        task_id: &str,
        source_id: Option<&str>,
    ) -> Result<Option<SourceRepairBinding>, OrbitError> {
        self.resolve_owner_published_candidate(task_id, source_id)
            .map_err(hold_operator_source_repair_refusal)
    }

    fn resolve_owner_published_candidate(
        &self,
        task_id: &str,
        source_id: Option<&str>,
    ) -> Result<Option<SourceRepairBinding>, OrbitError> {
        let task = self.get_task(task_id)?;
        if !self.task_run_is_local(&task) {
            return Ok(None);
        }
        let current = match &task.job_run_id {
            Some(id) => self.get_job_run_backend(id)?,
            None => None,
        };
        // Claimed and local-only deliveries retain their existing candidate
        // contracts; this path authenticates owner PR publications only.
        if current.as_ref().is_some_and(|run| {
            run.job_id != workflow::TASK_PR_PIPELINE_JOB
                || run.input.as_ref().is_some_and(|i| i["claimed"] == true)
        }) {
            return Ok(None);
        }
        let entries = self.get_task_history(task_id)?;
        let records = entries
            .iter()
            .filter(|e| e.event == PRESERVED)
            .map(|e| {
                if e.by != "system" {
                    return Err(refuse("untrusted publication preservation"));
                }
                parse_preserved(e).map(|record| (e.at, record))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let predecessor = || -> Result<String, OrbitError> {
            let current = current
                .as_ref()
                .ok_or_else(|| refuse("current execution missing"))?;
            let runs = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
                job_id: Some(workflow::TASK_PR_PIPELINE_JOB.into()),
                task_id: Some(task_id.into()),
                limit: Some(super::super::successor::SUCCESSOR_CHAIN_WINDOW),
                include_steps: false,
                ..Default::default()
            })?;
            if runs.first().is_none_or(|run| run.run_id != current.run_id) {
                return Err(refuse(
                    "newer execution makes publication association ambiguous",
                ));
            }
            // Runs held on a busy footprint after adopting the publication
            // changed nothing remote; they stand aside for the publisher
            // they adopted, and must all name it [ORB-24993].
            let mut adopted = Vec::new();
            for run in runs.iter().skip(1) {
                let Some(run) = self.get_job_run_backend(&run.run_id)? else {
                    return Err(refuse("published predecessor is missing"));
                };
                if let Some(source) = self.footprint_held_adopted_source(&run)? {
                    adopted.push(source);
                    continue;
                }
                if adopted.iter().any(|source| *source != run.run_id) {
                    return Err(refuse(
                        "a footprint-held attempt adopted another publication than its predecessor",
                    ));
                }
                return Ok(run.run_id);
            }
            Err(refuse("published predecessor is missing"))
        };
        // Setup can stamp the task and lose its reply before checkpointing
        // the prior id. Never skip a newer attempt to find an older publication.
        let recovered_source;
        let source_id: &str = match source_id {
            // The binding setup recorded may be a footprint-held run that only
            // adopted this publication [ORB-24993].
            Some(id) => {
                recovered_source = self.published_source_through_footprint_hold(id)?;
                &recovered_source
            }
            None if !records.is_empty() || task.github_pr_number().is_some() => {
                recovered_source = predecessor()?;
                &recovered_source
            }
            None => return Ok(None),
        };
        let source = self.get_job_run_backend(source_id)?;
        if source.as_ref().is_some_and(|run| {
            run.job_id
                .starts_with(workflow::CLAIMED_DELIVERY_LEAF_PREFIX)
                || run.input.as_ref().is_some_and(|i| i["claimed"] == true)
        }) {
            return Ok(None);
        }
        let state = self.read_run_state(source_id)?;
        let published = state.as_ref().is_some_and(|s| {
            ["push", "pr_open"]
                .iter()
                .any(|k| s.pipeline.get(k).is_some_and(|v| !v.is_null()))
        });
        let matching = records
            .iter()
            .filter(|(_, r)| r.source_run_id == source_id)
            .collect::<Vec<_>>();
        // An operator discarded this run's candidate and no requeue kept its
        // publication: the next run implements fresh, which the generic
        // candidate lookup does once it sees the discard. Only a stopped
        // source may be left behind this way.
        if matching.is_empty()
            && let Some(stopped) = source.as_ref()
            && entries.iter().any(|e| {
                e.event == orbit_types::task::CANDIDATE_DISCARDED_EVENT
                    && e.at >= stopped.created_at
            })
        {
            if stopped.finished_at.is_none()
                || run_owner_liveness(stopped) != RunOwnerLiveness::Stopped
            {
                return Err(refuse(
                    "a discarded publication's source execution is still live",
                ));
            }
            return Ok(None);
        }
        if matching.is_empty() && !published {
            if !records.is_empty() || task.github_pr_number().is_some() {
                // [ORB-20956] The discard predates the predecessor, but the
                // host records prove every attempt since then implemented
                // fresh because of it and published nothing.
                if let Some(current) = current.as_ref()
                    && self.discard_survives_unpublished_successors(
                        &task, current, source_id, &entries, &records,
                    )?
                {
                    return Ok(None);
                }
                // The immediate predecessor has no publication evidence. Its
                // liveness cannot identify the older publisher; require the
                // operator to select that source explicitly.
                return Err(refuse_operator_decision(format!(
                    "a published candidate predates the current binding; predecessor \
                     '{source_id}' has no publication evidence; stop any newer attempts, \
                     then select the older publishing run for explicit source \
                     repair (orbit.run action resume with source_repair), or discard \
                     the candidate (orbit task update {} --status backlog \
                     --discard-candidate) so the next run implements fresh",
                    task.id
                )));
            }
            return Ok(None);
        }
        if matching.len() != 1 {
            return Err(refuse_operator_decision(format!(
                "published candidate lacks a unique requeue binding; inspect explicit source \
                 repair for publishing run '{source_id}', or discard the candidate (orbit task \
                 update {} --status backlog --discard-candidate) so the next run implements fresh",
                task.id
            )));
        }
        if predecessor()? != source_id {
            return Err(refuse(
                "a newer owner attempt requires explicit source repair",
            ));
        }
        let source = source.ok_or_else(|| refuse("published source missing"))?;
        self.owner_source_identity(&source, &task)?;
        if !adoptable_published_source(&source)
            || self.recorded_final_recovery(task_id, source_id)?
                != Some(crate::application::task::FinalRecoveryOutcome::Requeued)
            || matching[0].1.schema_version != 1
            || !super::super::adoption::task_matches_before_crew_redraws(
                &task,
                &entries,
                matching[0].0,
                &matching[0].1.task_digest,
            )?
            || entries.iter().any(|e| {
                e.event == orbit_types::task::CANDIDATE_DISCARDED_EVENT && e.at >= source.created_at
            })
        {
            return Err(refuse_operator_decision(
                "published source is live, discarded, changed, or lacks an applied requeue",
            ));
        }
        if matching[0].1.failure_handoff {
            let state = self
                .read_run_state(source_id)?
                .ok_or_else(|| refuse("source checkpoints missing"))?;
            if state.failure_activity_checkpoint.is_none() {
                return Err(refuse("preserved failure-handoff receipt disappeared"));
            }
            let definition = self.source_repair_definition(&source)?;
            return self
                .observe_handoff_publication(source, task, state, definition)
                .map(|(_, _, binding)| Some(binding));
        }
        self.observe_published_source(source, task, false)
            .map(|(_, _, binding)| Some(binding))
    }

    /// [ORB-20956] Whether an operator's discard of a publication still
    /// governs `current`, although conclusively stopped attempts that
    /// published nothing ran between them, so the run implements fresh
    /// without another discard.
    ///
    /// Walks the task's bindings from `current` back through `source_id`.
    /// Each intervening attempt must be an owner-local single-task run that
    /// stopped, kept no preservation record, published nothing (no push, PR
    /// or publishing failure handoff), and whose authenticated host
    /// checkpoints show it set up on the next older binding and implemented
    /// fresh. The walk ends at the first attempt created after a discard
    /// recorded since the next older binding began: that binding is the
    /// discarded publisher, which must be a stopped owner-local run with
    /// publication evidence and no preservation record, and the attempt's
    /// fresh outcome must name it as the discarded source or have found no
    /// candidate at all. Anything else is unproven, and the caller keeps its
    /// refusal.
    fn discard_survives_unpublished_successors(
        &self,
        task: &Task,
        current: &JobRun,
        source_id: &str,
        entries: &[TaskHistoryEntry],
        records: &[(chrono::DateTime<Utc>, PreservedPublication)],
    ) -> Result<bool, OrbitError> {
        let runs = self
            .stores()
            .jobs()
            .list_job_runs_filtered(&JobRunQuery {
                task_id: Some(task.id.clone()),
                limit: Some(super::super::successor::SUCCESSOR_CHAIN_WINDOW),
                include_steps: false,
                ..Default::default()
            })?
            .into_iter()
            .filter(|run| workflow::is_delivery_leaf_job_by_claimed_prefix(&run.job_id))
            .collect::<Vec<_>>();
        if runs.first().is_none_or(|run| run.run_id != current.run_id)
            || runs.get(1).is_none_or(|run| run.run_id != source_id)
        {
            return Ok(false);
        }
        for pair in runs[1..].windows(2) {
            let [member, older] = pair else {
                return Ok(false);
            };
            let Some(member) = self.get_job_run_backend(&member.run_id)? else {
                return Ok(false);
            };
            let Some(resumed) =
                self.unpublished_fresh_attempt(task, &member, &older.run_id, records)?
            else {
                return Ok(false);
            };
            let discarded = entries.iter().any(|e| {
                e.event == orbit_types::task::CANDIDATE_DISCARDED_EVENT
                    && e.at >= older.created_at
                    && e.at < member.created_at
            });
            if !discarded {
                continue;
            }
            let names_discarded = resumed["reason_code"] == "candidate_discarded"
                && resumed["source_run_id"] == older.run_id.as_str();
            let found_none =
                resumed["reason_code"] == "no_candidate" && resumed["source_run_id"].is_null();
            let Some(publisher) = self.get_job_run_backend(&older.run_id)? else {
                return Ok(false);
            };
            let published = self
                .read_run_state(&publisher.run_id)?
                .is_some_and(|state| {
                    state.run_id == publisher.run_id
                        && ["push", "pr_open"]
                            .iter()
                            .any(|k| state.pipeline.get(k).is_some_and(|v| !v.is_null()))
                });
            return Ok((names_discarded || found_none)
                && published
                && self.owner_source_identity(&publisher, task).is_ok()
                && publisher.finished_at.is_some()
                && run_owner_liveness(&publisher) == RunOwnerLiveness::Stopped
                && !records
                    .iter()
                    .any(|(_, r)| r.source_run_id == publisher.run_id));
        }
        Ok(false)
    }

    /// The authenticated fresh `candidate_resume` output of `member`, when it
    /// is a conclusively stopped owner-local attempt set up on `older_id`
    /// that published nothing and left no commit checkpoint or uncommitted
    /// work; `None` otherwise.
    fn unpublished_fresh_attempt(
        &self,
        task: &Task,
        member: &JobRun,
        older_id: &str,
        records: &[(chrono::DateTime<Utc>, PreservedPublication)],
    ) -> Result<Option<Value>, OrbitError> {
        if self.owner_source_identity(member, task).is_err()
            || !matches!(member.state, JobRunState::Failed | JobRunState::Cancelled)
            || member.finished_at.is_none()
            || run_owner_liveness(member) != RunOwnerLiveness::Stopped
            || records
                .iter()
                .any(|(_, r)| r.source_run_id == member.run_id)
        {
            return Ok(None);
        }
        let Some(state) = self.read_run_state(&member.run_id)? else {
            return Ok(None);
        };
        // A failure handoff commits and publishes whatever the run left; only
        // its recorded no-candidate outcome published nothing. A commit
        // checkpoint is candidate evidence the next adoption would not resume,
        // so it fails the same bar `observe_successor_member` applies.
        if state.run_id != member.run_id
            || state.job_id != member.job_id
            || super::super::successor::PUBLICATION_KEYS
                .iter()
                .any(|k| state.pipeline.get(k).is_some_and(|v| !v.is_null()))
            || member
                .steps
                .iter()
                .any(|s| super::super::successor::PUBLICATION_KEYS.contains(&s.target_id.as_str()))
            || state
                .failure_activity_checkpoint
                .as_ref()
                .is_some_and(|c| c.output["decision"] != "no_candidate")
        {
            return Ok(None);
        }
        // Work the attempt left uncommitted is a resume candidate the next
        // run's new worktree would not carry; unreadable changes are unproven.
        match self.retained_candidate(task, Some(&member.run_id), Some(member)) {
            crate::application::task::RetainedCandidate::Present {
                changed_paths: Ok(paths),
                ..
            } if paths.is_empty() => {}
            crate::application::task::RetainedCandidate::Absent { .. } => {}
            crate::application::task::RetainedCandidate::Present { .. } => return Ok(None),
        }
        let definition = self.source_repair_definition(member)?;
        let checkpoint = |id, action| {
            super::super::observation::authenticated_checkpoint(&definition, &state, id, action)
                .ok()
        };
        let (Some(worktree), Some(resumed)) = (
            checkpoint("worktree", "worktree_setup"),
            checkpoint("resume_candidate", "candidate_resume"),
        ) else {
            return Ok(None);
        };
        let fresh = worktree["prior_job_run_id"] == older_id
            && worktree["prior_foreign_run"].is_null()
            && resumed["phase"] == "candidate_resume"
            && resumed["outcome"] == "fresh"
            && resumed["implement"] == true
            && resumed["base_sha"] == worktree["base_sha"]
            && resumed["reused_branch"].is_null()
            && resumed["prior_pull_request"].is_null();
        Ok(fresh.then(|| resumed.clone()))
    }

    /// Publication authority is the successful host adoption, never a caller's input.
    pub(crate) fn owner_publication_binding(
        &self,
        run_id: &str,
    ) -> Result<Option<SourceRepairBinding>, OrbitError> {
        self.read_owner_publication_binding(run_id, true)
    }

    pub(crate) fn read_owner_publication_binding(
        &self,
        run_id: &str,
        active: bool,
    ) -> Result<Option<SourceRepairBinding>, OrbitError> {
        let Some(run) = self.get_job_run_backend(run_id)? else {
            return Ok(None);
        };
        let Some(state) = self.read_run_state(run_id)? else {
            return Ok(None);
        };
        let Some(output) = state.pipeline.get("resume_candidate") else {
            return Ok(None);
        };
        let Some(raw) = output.get("published_binding") else {
            return Ok(None);
        };
        let binding: SourceRepairBinding =
            serde_json::from_value(raw.clone()).map_err(|e| refuse(e.to_string()))?;
        let task = self.get_task(&binding.task_id)?;
        self.owner_source_identity(&run, &task)?;
        let job = self.source_repair_definition(&run)?;
        let index = job
            .steps
            .iter()
            .position(|s| s.id == "resume_candidate")
            .ok_or_else(|| refuse("adoption step absent"))?;
        let authenticated = matches!(&job.steps[index].body, JobV2StepBody::Target(t) if t.activity_name.as_deref() == Some("candidate_resume") && matches!(&t.spec, ActivityV2Spec::Deterministic(s) if s.action == "candidate_resume"));
        if !authenticated
            || state.run_id != run_id
            || state.job_id != run.job_id
            || state.step_states.get(&(index as u32)) != Some(&JobRunState::Success)
            || state.step_output(index as u32) != Some(output)
            || (active
                && (!matches!(run.state, JobRunState::Pending | JobRunState::Running)
                    || task.job_run_id.as_deref() != Some(run_id)
                    || !matches!(task.status, TaskStatus::InProgress | TaskStatus::Review)))
            || binding.schema_version != 1
            || binding.source_run_id == run_id
            // This run's commit may widen the task's selectors to files the
            // adopted candidate changed (an auto-fix repair the task did not
            // declare); only those system widenings are set aside.
            || (active
                && !super::super::adoption::admitted_task_matches(
                    &task,
                    &self.get_task_history(&task.id)?,
                    run_id,
                    run.created_at,
                    &binding.task_digest,
                )?)
            || output["phase"] != "candidate_resume"
            || !matches!(
                output["outcome"].as_str(),
                Some("resumed_carried" | "resumed_repaired")
            )
            || output["source_branch"] != binding.branch
            || output["source_sha"] != binding.candidate_head()
            || output["base_sha"] != state.pipeline["worktree"]["base_sha"]
            || output["prior_pull_request_head"] != binding.branch
            || output["reused_branch"] != binding.branch
            || output["reused_head_sha"] != binding.head_sha
            || output["prior_pull_request"] != binding.pull_request
            || output["source_run_id"] != binding.source_run_id
        {
            return Err(refuse("published adoption authority changed"));
        }
        let source = self
            .get_job_run_backend(&binding.source_run_id)?
            .ok_or_else(|| refuse("source disappeared"))?;
        let source_state = self
            .read_run_state(&binding.source_run_id)?
            .ok_or_else(|| refuse("source checkpoints disappeared"))?;
        let definition = self.source_repair_definition(&source)?;
        if binding.workspace_id != self.workspace_id()?
            || run_evidence_digest(&source, &source_state, &definition)? != binding.source_digest
        {
            return Err(refuse("immutable published source changed"));
        }
        Ok(Some(binding))
    }
}
