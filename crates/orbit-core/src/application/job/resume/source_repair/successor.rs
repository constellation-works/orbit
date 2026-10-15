//! A newer interrupted run is acknowledged explicitly, never silently skipped.
use super::*;
use orbit_types::workflow::SourceRepairSuccessor;

impl OrbitRuntime {
    pub(super) fn repairable_published_source(
        &self,
        source: &JobRun,
        task: &str,
    ) -> Result<bool, OrbitError> {
        Ok(source.state == JobRunState::Cancelled
            || (source.state == JobRunState::Failed
                && self.recorded_final_recovery(task, &source.run_id)?
                    == Some(crate::application::task::FinalRecoveryOutcome::Requeued)))
    }

    pub(super) fn observe_source_successor(
        &self,
        source: &JobRun,
        task: &Task,
    ) -> Result<SourceRepairSuccessor, OrbitError> {
        let id = task
            .job_run_id
            .as_deref()
            .ok_or_else(|| refuse("newer task binding missing"))?;
        let successor = self
            .get_job_run_backend(id)?
            .ok_or_else(|| refuse("newer execution missing"))?;
        if successor.created_at <= source.created_at
            || successor.state != JobRunState::Cancelled
            || successor.finished_at.is_none()
            || run_owner_liveness(&successor) != RunOwnerLiveness::Stopped
            || successor.job_id != "task_pr_pipeline"
            || successor.executed_on != source.executed_on
            || successor.input.as_ref().is_none_or(|i| {
                i["claimed"] == true || carried_task_ids(i) != BTreeSet::from([task.id.clone()])
            })
            || self.stores().jobs().local_pull_for_run(id)?.is_some()
        {
            return Err(refuse(
                "newer binding must be a conclusively stopped cancelled owner-local replacement",
            ));
        }
        let state = self
            .read_run_state(id)?
            .ok_or_else(|| refuse("newer checkpoints missing"))?;
        // No candidate produced by another attempt may be discarded to select an
        // older head. Even failed/partial publication is an unknown outcome.
        if state.run_id != id
            || state.job_id != successor.job_id
            || ["commit", "push", "pr_open", "landing_push"]
                .iter()
                .any(|k| state.pipeline.get(k).is_some_and(|v| !v.is_null()))
            || successor.steps.iter().any(|s| {
                ["commit", "push", "pr_open", "landing_push"].contains(&s.target_id.as_str())
            })
        {
            return Err(refuse(
                "newer execution has candidate/publication evidence; reconcile it explicitly",
            ));
        }
        let definition = self.source_repair_definition(&successor)?;
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
                && protected.iter().any(|(_, id)| *id == step_id)
            {
                return Err(refuse(
                    "newer execution may have written a candidate before checkpointing; reconcile its outcome",
                ));
            }
        }
        Ok(SourceRepairSuccessor {
            run_id: id.into(),
            digest: digest(&json!({"run":successor,"checkpoints":state,"definition":definition}))?,
        })
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
