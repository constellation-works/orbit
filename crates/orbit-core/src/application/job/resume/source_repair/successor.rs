//! A newer interrupted run is acknowledged explicitly, never silently skipped.
use super::*;
use crate::application::task::{FinalRecoveryOutcome, RetainedCandidate};
use orbit_types::workflow::SourceRepairSuccessor;

/// The newest runs carrying the task read to find every binding between the
/// source and the current one. A source older than this window is refused.
const SUCCESSOR_CHAIN_WINDOW: usize = 64;

/// The candidate and publication checkpoints a newer execution must not hold.
const PUBLICATION_KEYS: [&str; 4] = ["commit", "push", "pr_open", "landing_push"];

/// A run of this job can bind the task. A gate or auto-dispatch wrapper also
/// carries the task, but its delivery child is the binding.
fn binds_task(job_id: &str) -> bool {
    matches!(job_id, "task_pr_pipeline" | "task_local_pipeline")
        || job_id.starts_with("task_claimed_")
}

impl OrbitRuntime {
    pub(super) fn repairable_published_source(
        &self,
        source: &JobRun,
        task: &str,
    ) -> Result<bool, OrbitError> {
        Ok(
            matches!(source.state, JobRunState::Cancelled | JobRunState::Success)
                || (source.state == JobRunState::Failed
                    && matches!(
                        self.recorded_final_recovery(task, &source.run_id)?,
                        Some(FinalRecoveryOutcome::Requeued | FinalRecoveryOutcome::Escalated { .. })
                    ))
                // A resume's applied repair settles nothing on the task; its
                // stopped failure-hook publication is repairable only through
                // the authenticated owner lineage.
                || self.resumed_recovery(source, task)?.is_some(),
        )
    }

    /// Acknowledge every binding newer than `source`, up to the task's
    /// current one. Each must be a conclusively stopped owner-local
    /// replacement that produced no candidate or publication; the digest
    /// covers them all, so a later change to any member invalidates it.
    pub(super) fn observe_source_successor(
        &self,
        source: &JobRun,
        task: &Task,
    ) -> Result<SourceRepairSuccessor, OrbitError> {
        if !self.task_run_is_local(task) {
            return Err(refuse("newer task binding belongs to another machine"));
        }
        let id = task
            .job_run_id
            .as_deref()
            .ok_or_else(|| refuse("newer task binding missing"))?;
        let current = self
            .get_job_run_backend(id)?
            .ok_or_else(|| refuse("newer execution missing"))?;
        let mut members = vec![self.observe_successor_member(source, task, &current)?];
        for older in self.successor_chain(source, task, &current)? {
            members.push(self.observe_successor_member(source, task, &older)?);
        }
        members.reverse();
        // A single successor keeps the digest bindings recorded before chains.
        let digest = match members.as_slice() {
            [only] => digest(only)?,
            chain => digest(&json!({ "chain": chain }))?,
        };
        Ok(SourceRepairSuccessor {
            run_id: id.into(),
            digest,
        })
    }

    /// The bindings strictly between `source` and `current`, newest first,
    /// from the task's bounded run history.
    fn successor_chain(
        &self,
        source: &JobRun,
        task: &Task,
        current: &JobRun,
    ) -> Result<Vec<JobRun>, OrbitError> {
        let runs = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            task_id: Some(task.id.clone()),
            limit: Some(SUCCESSOR_CHAIN_WINDOW),
            include_steps: false,
            ..Default::default()
        })?;
        let Some(source_at) = runs.iter().position(|run| run.run_id == source.run_id) else {
            return Err(refuse(
                "newer executions exceed the bounded inspection window",
            ));
        };
        let Some(current_at) = runs.iter().position(|run| run.run_id == current.run_id) else {
            return Err(refuse(
                "newer execution makes publication association ambiguous",
            ));
        };
        if current_at >= source_at {
            return Err(refuse(
                "newer execution makes publication association ambiguous",
            ));
        }
        runs[current_at + 1..source_at]
            .iter()
            .filter(|run| binds_task(&run.job_id))
            .map(|run| {
                if run.created_at >= current.created_at {
                    return Err(refuse(
                        "newer execution makes publication association ambiguous",
                    ));
                }
                self.get_job_run_backend(&run.run_id)?
                    .ok_or_else(|| refuse("newer execution missing"))
            })
            .collect()
    }

    /// One newer binding's acknowledged evidence, or the reason it cannot be
    /// set aside.
    fn observe_successor_member(
        &self,
        source: &JobRun,
        task: &Task,
        successor: &JobRun,
    ) -> Result<Value, OrbitError> {
        let id = successor.run_id.as_str();
        if successor.created_at <= source.created_at
            || !matches!(
                successor.state,
                JobRunState::Cancelled | JobRunState::Failed
            )
            || successor.finished_at.is_none()
            || run_owner_liveness(successor) != RunOwnerLiveness::Stopped
            || successor.job_id != "task_pr_pipeline"
            || successor.executed_on != source.executed_on
            || successor.input.as_ref().is_none_or(|i| {
                i["claimed"] == true || carried_task_ids(i) != BTreeSet::from([task.id.clone()])
            })
            || self.stores().jobs().local_pull_for_run(id)?.is_some()
        {
            return Err(refuse(
                "newer binding must be a conclusively stopped cancelled or failed owner-local replacement",
            ));
        }
        let failed = successor.state == JobRunState::Failed;
        // A failed run's outcome is settled only by its final recovery.
        if failed
            && !matches!(
                self.recorded_final_recovery(&task.id, id)?,
                Some(FinalRecoveryOutcome::Requeued | FinalRecoveryOutcome::Escalated { .. })
            )
        {
            return Err(refuse(
                "newer failed execution has no settled final recovery",
            ));
        }
        let state = self
            .read_run_state(id)?
            .ok_or_else(|| refuse("newer checkpoints missing"))?;
        // No candidate produced by another attempt may be discarded to select an
        // older head. Even failed/partial publication is an unknown outcome.
        if state.run_id != id
            || state.job_id != successor.job_id
            || PUBLICATION_KEYS
                .iter()
                .any(|k| state.pipeline.get(k).is_some_and(|v| !v.is_null()))
            || successor
                .steps
                .iter()
                .any(|s| PUBLICATION_KEYS.contains(&s.target_id.as_str()))
            // A failure handoff commits and publishes whatever the run left;
            // only its recorded no-candidate outcome published nothing.
            || (failed
                && state
                    .failure_activity_checkpoint
                    .as_ref()
                    .is_some_and(|c| c.output["decision"] != "no_candidate"))
        {
            return Err(refuse(
                "newer execution has candidate/publication evidence; reconcile it explicitly",
            ));
        }
        let definition = self.source_repair_definition(successor)?;
        let protected = definition.steps.iter().enumerate().filter(|(_, step)| {
            matches!(&step.body, JobV2StepBody::Target(t) if matches!(&t.spec,
                ActivityV2Spec::Deterministic(s) if ["git_commit", "git_push", "pr_open"].contains(&s.action.as_str())))
        }).map(|(index, step)| (index as u32, step.id.as_str())).collect::<Vec<_>>();
        if protected
            .iter()
            .any(|(index, _)| state.step_states.contains_key(index))
        {
            return Err(refuse(
                "newer execution reached a candidate/publication action",
            ));
        }
        // A failed run is set aside only when it stopped at or before
        // candidate adoption: anything later may have produced source.
        let before_adoption = if failed {
            let adoption = definition
                .steps
                .iter()
                .position(|step| {
                    matches!(&step.body, JobV2StepBody::Target(t) if matches!(&t.spec,
                        ActivityV2Spec::Deterministic(s) if s.action == "candidate_resume"))
                })
                .ok_or_else(|| refuse("newer failed execution has no adoption boundary"))?;
            let adoption_id = definition.steps[adoption].id.as_str();
            // Step rows are keyed by step id; a worker's job-level diagnostic
            // row names the job instead.
            let later = definition.steps[adoption + 1..]
                .iter()
                .map(|step| step.id.as_str())
                .collect::<BTreeSet<_>>();
            if state
                .step_states
                .keys()
                .any(|index| *index as usize > adoption)
                || state.step_states.get(&(adoption as u32)) == Some(&JobRunState::Success)
                || successor.steps.iter().any(|s| {
                    later.contains(s.target_id.as_str())
                        || (s.target_id.as_str() == adoption_id && s.state == JobRunState::Success)
                })
            {
                return Err(refuse(
                    "newer failed execution passed candidate adoption; resume or discard its candidate explicitly",
                ));
            }
            Some(
                definition.steps[..=adoption]
                    .iter()
                    .map(|step| step.id.as_str())
                    .collect::<BTreeSet<_>>(),
            )
        } else {
            None
        };
        // A crash can precede checkpointing. Host step-start audit rows are
        // therefore evidence of a possible write even when no output exists.
        let rows = self.list_v2_audit_events(orbit_store::contracts::V2AuditEventFilter {
            workspace_id: self.workspace_id()?,
            run_id: Some(id.into()),
            source: Some("v2_envelope".into()),
            body_kinds: vec!["step_started".into()],
            limit: Some(257),
            ..Default::default()
        })?;
        if rows.len() > 256 {
            return Err(refuse(
                "newer execution audit exceeds the bounded inspection window",
            ));
        }
        for row in rows {
            let event: orbit_types::workflow::activity_job::V2AuditEvent =
                serde_json::from_str(&row.payload_json)
                    .map_err(|e| refuse(format!("newer execution audit is unreadable: {e}")))?;
            if let orbit_types::workflow::activity_job::V2AuditEventKind::StepStarted { step_id } =
                event.kind
            {
                if protected.iter().any(|(_, id)| *id == step_id) {
                    return Err(refuse(
                        "newer execution may have written a candidate before checkpointing; reconcile its outcome",
                    ));
                }
                // Nested and later steps all start after adoption.
                if before_adoption
                    .as_ref()
                    .is_some_and(|steps| !steps.contains(step_id.as_str()))
                {
                    return Err(refuse(
                        "newer failed execution may have started implementation before checkpointing; resume or discard its candidate explicitly",
                    ));
                }
            }
        }
        // Work a failed run left uncommitted is a resume candidate.
        if failed {
            match self.retained_candidate(task, Some(id), Some(successor)) {
                RetainedCandidate::Present {
                    changed_paths: Ok(paths),
                    ..
                } if paths.is_empty() => {}
                RetainedCandidate::Absent { .. } => {}
                RetainedCandidate::Present { .. } => {
                    return Err(refuse(
                        "newer failed execution left uncommitted work; resume or discard its candidate explicitly",
                    ));
                }
            }
        }
        Ok(json!({"run":successor,"checkpoints":state,"definition":definition}))
    }

    pub(super) fn source_successor_unchanged(
        &self,
        binding: &SourceRepairBinding,
    ) -> Result<bool, OrbitError> {
        let Some(expected) = &binding.superseded else {
            return Ok(true);
        };
        let source = self
            .get_job_run_backend(&binding.source_run_id)?
            .ok_or_else(|| refuse("source disappeared"))?;
        let mut task = self.get_task(&binding.task_id)?;
        task.job_run_id = Some(expected.run_id.clone());
        Ok(self.observe_source_successor(&source, &task)? == *expected)
    }
}
