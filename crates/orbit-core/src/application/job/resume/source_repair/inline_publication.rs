//! Fresh operator authority over a kept inline publication, never completion reuse.
use super::observation::authenticated_checkpoint;
use super::*;
use orbit_types::workflow::SourceRepairSuccessor;

const LINEAGE_WINDOW: usize = 64;

pub(super) fn kept_inline_publication(state: &PipelineState) -> bool {
    state
        .failure_activity_checkpoint
        .as_ref()
        .is_some_and(|receipt| {
            matches!(
                receipt.output["decision"].as_str(),
                Some("review_completion_failure" | "landing_review_failure")
            )
        })
}

pub(super) fn requires_fresh_rework_authority(source: &JobRun, state: &PipelineState) -> bool {
    source.state == JobRunState::Success
        || (source.state == JobRunState::Failed && kept_inline_publication(state))
}

/// A kept receipt certifies scope and preservation only. The ordinary host
/// commit/review/push/PR chain independently authenticates the publication.
pub(super) fn authenticate_kept_inline_receipt(
    source: &JobRun,
    task: &Task,
    state: &PipelineState,
    definition: &JobV2,
) -> Result<(), OrbitError> {
    let receipt = state
        .failure_activity_checkpoint
        .as_ref()
        .ok_or_else(|| refuse("kept inline publication receipt missing"))?;
    let output = &receipt.output;
    let failed = definition
        .steps
        .iter()
        .position(|step| step.id == receipt.failed_step_id)
        .ok_or_else(|| refuse("kept publication failed step absent from definition"))?;
    let opened = definition
        .steps
        .iter()
        .position(|step| step.id == "pr_open")
        .ok_or_else(|| refuse("kept publication lacks original PR step"))?;
    if definition.failure_activity.as_deref() != Some("pr_failure_handoff")
        || !definition.resolved_failure_activity.as_ref().is_some_and(|activity|
            matches!(&activity.spec, ActivityV2Spec::Deterministic(spec) if spec.action == "pr_failure_handoff"))
        || receipt.activity_name != "pr_failure_handoff"
        || !kept_inline_publication(state)
        || field(output, "phase")? != "failure_handoff"
        || field(output, "failed_step_id")? != receipt.failed_step_id
        || field(output, "handoff_run_id")? != source.run_id
        || field(output, "task_id")? != task.id
        || output["candidate_preserved"] != true
        || failed <= opened
        || state.step_states.get(&(failed as u32)) == Some(&JobRunState::Success)
    {
        return Err(refuse("kept inline publication ownership or failure proof disagrees"));
    }
    if !task.spec_digest_matches(field(output, "task_spec_digest")?) {
        return Err(refuse(
            "task specification changed after inline publication",
        ));
    }
    Ok(())
}

impl OrbitRuntime {
    /// Bind every task delivery since the original publisher, including
    /// retries that reused its checkpoints without rebinding the task. Older
    /// retry ancestors are also retained. Only the admitted repair itself is
    /// excluded when rechecking this evidence before source effects.
    pub(super) fn observe_inline_repair_lineage(
        &self,
        source: &JobRun,
        task: &Task,
        admitted: Option<&str>,
    ) -> Result<SourceRepairSuccessor, OrbitError> {
        let bound = task
            .job_run_id
            .as_deref()
            .ok_or_else(|| refuse("task binding missing"))?;
        let runs = self
            .stores()
            .jobs()
            .list_job_runs_filtered(&JobRunQuery {
                task_id: Some(task.id.clone()),
                limit: Some(LINEAGE_WINDOW + 1),
                include_steps: true,
                ..Default::default()
            })?
            .into_iter()
            .filter(|run| Some(run.run_id.as_str()) != admitted)
            .take(LINEAGE_WINDOW)
            .collect::<Vec<_>>();
        let source_at = runs
            .iter()
            .position(|run| run.run_id == source.run_id)
            .ok_or_else(|| {
                refuse("inline publication exceeds the complete lineage inspection window")
            })?;
        let source_state = self
            .read_run_state(&source.run_id)?
            .ok_or_else(|| refuse("source checkpoints missing"))?;
        let definition = self.source_repair_definition(source)?;
        // Do not let a malformed retry sort behind its publisher and disappear
        // from the newer-run slice (including ties resolved by store ordering).
        for run in runs.iter().filter(|run| {
            run.run_id != source.run_id
                && run.retry_source_run_id.is_some()
                && run.created_at <= source.created_at
        }) {
            if run.retry_source_run_id.as_deref() == Some(source.run_id.as_str())
                || self
                    .read_run_state(&run.run_id)?
                    .is_some_and(|state| state.pipeline["worktree"]["job_run_id"] == source.run_id)
            {
                return Err(refuse(
                    "inline publication retry lineage ordering is ambiguous",
                ));
            }
        }
        // A failed completion may have left uncommitted repairs even without
        // a later retry. They must not be dropped when adopting the PR head.
        let clean = match self.retained_candidate(task, Some(&source.run_id), Some(source)) {
            crate::application::task::RetainedCandidate::Absent { .. } => true,
            crate::application::task::RetainedCandidate::Present {
                changed_paths: Ok(paths),
                ..
            } => paths.is_empty(),
            _ => false,
        };
        if !clean {
            return Err(refuse(
                "inline publication left uncommitted work; reconcile its candidate explicitly",
            ));
        }
        let mut evidence = Vec::new();
        let mut newest_binding = source.run_id.as_str();
        for run in runs[..source_at].iter().rev() {
            if !is_delivery(&run.job_id) {
                continue;
            }
            if run.created_at <= source.created_at {
                return Err(refuse("inline publication lineage ordering is ambiguous"));
            }
            let state = self
                .read_run_state(&run.run_id)?
                .ok_or_else(|| refuse("inline publication lineage checkpoints missing"))?;
            if run.retry_source_run_id.is_some()
                && state.pipeline["worktree"]["job_run_id"] == source.run_id
            {
                // Require a complete acyclic parent chain back to this exact
                // publisher; another run's copied prefix grants no authority.
                let mut child = run;
                let mut visited = BTreeSet::new();
                let mut lineage = vec![run];
                while child.run_id != source.run_id {
                    if !visited.insert(child.run_id.as_str()) {
                        return Err(refuse("inline publication retry lineage repeats a run"));
                    }
                    let parent = child
                        .retry_source_run_id
                        .as_deref()
                        .and_then(|id| runs.iter().find(|parent| parent.run_id == id))
                        .ok_or_else(|| refuse("inline publication retry lineage is incomplete"))?;
                    if !is_delivery(&parent.job_id) {
                        return Err(refuse("inline publication retry parent is not a delivery"));
                    }
                    if parent.created_at >= child.created_at {
                        return Err(refuse(
                            "inline publication retry lineage ordering disagrees",
                        ));
                    }
                    lineage.push(parent);
                    child = parent;
                }
                lineage.reverse();
                evidence.push(self.observe_inline_completion_retry(
                    task,
                    (&source_state, &definition),
                    run,
                    &state,
                    &lineage,
                )?);
            } else {
                evidence.push(self.observe_successor_member(source, task, run)?);
                newest_binding = &run.run_id;
            }
        }
        if bound != newest_binding {
            return Err(refuse(
                "inline publication task binding is not the newest acknowledged owner",
            ));
        }
        let mut ancestor = source;
        let mut visited = BTreeSet::from([source.run_id.as_str()]);
        while let Some(parent_id) = ancestor.retry_source_run_id.as_deref() {
            let parent = runs
                .iter()
                .find(|run| run.run_id == parent_id)
                .ok_or_else(|| refuse("inline publication ancestor lineage is incomplete"))?;
            if !visited.insert(parent.run_id.as_str()) || parent.created_at >= ancestor.created_at {
                return Err(refuse("inline publication ancestor lineage disagrees"));
            }
            self.require_stopped_inline_lineage_member(parent, task)?;
            let state = self
                .read_run_state(parent_id)?
                .ok_or_else(|| refuse("inline publication ancestor checkpoints missing"))?;
            evidence.push(json!({"ancestor":parent,"state":state,
                "definition":self.source_repair_definition(parent)?}));
            ancestor = parent;
        }
        if self.get_task_history(&task.id)?.iter().any(|entry| {
            entry.event == orbit_types::task::CANDIDATE_DISCARDED_EVENT
                && entry.at >= source.created_at
        }) {
            return Err(refuse("an operator discarded this publication's candidate"));
        }
        Ok(SourceRepairSuccessor {
            run_id: bound.into(),
            digest: digest(&evidence)?,
        })
    }

    fn require_stopped_inline_lineage_member(
        &self,
        run: &JobRun,
        task: &Task,
    ) -> Result<(), OrbitError> {
        self.owner_source_identity(run, task)?;
        if !matches!(
            run.state,
            JobRunState::Failed
                | JobRunState::Cancelled
                | JobRunState::Held
                | JobRunState::Timeout
                | JobRunState::Interrupted
                | JobRunState::Success
        ) || run.finished_at.is_none()
            || run_owner_liveness(run) != RunOwnerLiveness::Stopped
        {
            return Err(refuse(
                "inline publication lineage contains an active or unsettled run",
            ));
        }
        Ok(())
    }

    fn observe_inline_completion_retry(
        &self,
        task: &Task,
        publication: (&PipelineState, &JobV2),
        retry: &JobRun,
        state: &PipelineState,
        lineage: &[&JobRun],
    ) -> Result<Value, OrbitError> {
        let (original, definition) = publication;
        self.require_stopped_inline_lineage_member(retry, task)?;
        for (id, action) in [
            ("worktree", "worktree_setup"),
            ("commit", "git_commit"),
            ("prepare_branch", "pr_prepare"),
            ("sync_base", "git_rebase"),
            ("push", "git_push"),
            ("pr_open", "pr_open"),
        ] {
            if authenticated_checkpoint(definition, state, id, action)?
                != authenticated_checkpoint(definition, original, id, action)?
            {
                return Err(refuse(
                    "completion retry changed original publication checkpoints",
                ));
            }
        }
        let failed = original
            .failure_activity_checkpoint
            .as_ref()
            .ok_or_else(|| refuse("original inline failure receipt missing"))?
            .failed_step_id
            .as_str();
        let completion = definition.steps.iter().find(|step| step.id == failed)
            .is_some_and(|step| matches!(&step.body, JobV2StepBody::Target(target)
                if matches!(&target.spec, ActivityV2Spec::Deterministic(spec) if spec.action == "pr_complete")));
        let failed_index = definition
            .steps
            .iter()
            .position(|step| step.id == failed)
            .ok_or_else(|| refuse("completion retry failed step absent"))?
            as u32;
        let history = self
            .get_task_history(&task.id)?
            .into_iter()
            .filter(|entry| entry.at >= retry.created_at && Some(entry.at) <= retry.finished_at)
            .collect::<Vec<_>>();
        let grant = self.authenticate_inline_retry_budget(task, original, retry, state, lineage)?;
        if !completion
            || retry.state == JobRunState::Success
            || state.run_id != retry.run_id
            || state.job_id != retry.job_id
            || digest(&self.source_repair_definition(retry)?)? != digest(definition)?
            || state.final_recovery != original.final_recovery
            || state.rebase_recovery_checkpoints != original.rebase_recovery_checkpoints
            || state.child_dispatches != original.child_dispatches
            || state.step_states.iter().any(|(index, status)| {
                *index != failed_index
                    && (original.step_states.get(index) != Some(status)
                        || state.step_output(*index) != original.step_output(*index))
            })
            || retry
                .input
                .as_ref()
                .is_some_and(|input| input.get(SOURCE_REPAIR_KEY).is_some())
            || orbit_types::workflow::pr_completion_has_mutation_provenance(&history)
            || retry
                .steps
                .iter()
                .any(|step| step.target_id != failed && step.target_id != retry.job_id)
            || state.pipeline.as_object().is_none_or(|outputs| {
                outputs.iter().any(|(key, value)| {
                    !value.is_null() && original.pipeline.get(key) != Some(value)
                })
            })
            || state
                .step_outputs
                .keys()
                .chain(state.step_output_pointers.keys())
                .any(|index| {
                    state.step_output(*index).is_some_and(|output| {
                        !output.is_null() && original.step_output(*index) != Some(output)
                    })
                })
        {
            return Err(refuse(
                "completion retry may have changed the original publication; reconcile its candidate explicitly",
            ));
        }
        let receipt_origin = if let Some(checkpoint) = &state.failure_activity_checkpoint {
            if checkpoint.failed_step_id != failed {
                return Err(refuse(
                    "completion retry receipt names a different failed step",
                ));
            }
            // A plain resume can inherit its parent's failure hook. Authenticate
            // the complete origin, then require uninterrupted inheritance along
            // this exact chain. A run id in an agent's output alone proves nothing.
            let owner_id = field(&checkpoint.output, "handoff_run_id")?;
            let owner_at = lineage
                .iter()
                .position(|run| run.run_id == owner_id)
                .ok_or_else(|| refuse("completion retry receipt owner is outside its lineage"))?;
            let owner = lineage[owner_at];
            let owner_state = self
                .read_run_state(owner_id)?
                .ok_or_else(|| refuse("completion retry receipt origin checkpoints missing"))?;
            authenticate_kept_inline_receipt(owner, task, &owner_state, definition)?;
            for member in &lineage[owner_at..] {
                let inherited = self.read_run_state(&member.run_id)?.ok_or_else(|| {
                    refuse("completion retry receipt lineage checkpoints missing")
                })?;
                if inherited.failure_activity_checkpoint.as_ref() != Some(checkpoint) {
                    return Err(refuse("completion retry receipt inheritance disagrees"));
                }
            }
            let receipt = &state
                .failure_activity_checkpoint
                .as_ref()
                .ok_or_else(|| refuse("retry receipt missing"))?
                .output;
            let original_receipt = &original
                .failure_activity_checkpoint
                .as_ref()
                .ok_or_else(|| refuse("original receipt missing"))?
                .output;
            if receipt["pr_number"] != original_receipt["pr_number"]
                || receipt["pr_url"] != original_receipt["pr_url"]
            {
                return Err(refuse("completion retry kept a different publication"));
            }
            json!({"run":owner,"state":owner_state})
        } else {
            Value::Null
        };
        let rows = self.list_v2_audit_events(orbit_store::contracts::V2AuditEventFilter {
            workspace_id: self.workspace_id()?,
            run_id: Some(retry.run_id.clone()),
            source: Some("v2_envelope".into()),
            body_kinds: vec!["step_started".into()],
            limit: Some(257),
            ..Default::default()
        })?;
        if rows.len() > 256 {
            return Err(refuse("completion retry audit exceeds inspection window"));
        }
        if !grant.is_null() && rows.is_empty() {
            return Err(refuse("completion retry audit evidence missing"));
        }
        for row in &rows {
            let event: orbit_types::workflow::activity_job::V2AuditEvent =
                serde_json::from_str(&row.payload_json).map_err(|error| {
                    refuse(format!("completion retry audit unreadable: {error}"))
                })?;
            if event.envelope.run_id != retry.run_id
                || !matches!(event.kind, orbit_types::workflow::activity_job::V2AuditEventKind::StepStarted { step_id } if step_id == failed)
            {
                return Err(refuse(
                    "completion retry started source work; reconcile its candidate explicitly",
                ));
            }
        }
        let audit = rows.iter().map(|row| &row.payload_json).collect::<Vec<_>>();
        Ok(
            json!({"run":retry,"state":state,"definition":definition,"audit":audit,"history":history,
                "grant":grant,"receipt_origin":receipt_origin}),
        )
    }

    /// A supported repair grant can renew completion's budget without ever
    /// reaching a repair agent. Authenticate its host ledger independently of
    /// the copied input, and permit only the reset at that recorded origin.
    fn authenticate_inline_retry_budget(
        &self,
        task: &Task,
        original: &PipelineState,
        retry: &JobRun,
        state: &PipelineState,
        lineage: &[&JobRun],
    ) -> Result<Value, OrbitError> {
        let marker_key = crate::application::job::resume::OUT_OF_RUN_AUTO_FIX_KEY;
        let marker = retry.input.as_ref().and_then(|input| input.get(marker_key));
        let parent = lineage
            .iter()
            .rev()
            .nth(1)
            .ok_or_else(|| refuse("completion retry parent missing"))?;
        let parent_state = self
            .read_run_state(&parent.run_id)?
            .ok_or_else(|| refuse("completion retry parent checkpoints missing"))?;
        let Some(marker) = marker else {
            if parent
                .input
                .as_ref()
                .is_some_and(|input| input.get(marker_key).is_some())
                || state.completion_deadlines != parent_state.completion_deadlines
            {
                return Err(refuse(
                    "completion retry changed its budget without a supported grant",
                ));
            }
            return Ok(Value::Null);
        };
        let pr = field(&original.pipeline["pr_open"], "pr_number")?;
        if field(marker, "task_id")? != task.id
            || field(marker, "pr_number")? != pr
            || field(marker, "head_sha")? != field(&original.pipeline["push"], "local_sha")?
            || !matches!(
                field(marker, "blocker")?,
                "failed_checks" | "conflict" | "landing_review"
            )
            || marker
                .get("failed_checks")
                .and_then(Value::as_array)
                .is_none_or(|checks| checks.iter().any(|check| check.as_str().is_none()))
            || marker
                .get("drain_run_id")
                .is_none_or(|id| !id.is_null() && id.as_str().is_none_or(str::is_empty))
            || retry.input.as_ref().is_none_or(|input| {
                input["auto_fix"] != true || input["footprint_deferred"] != true
            })
        {
            return Err(refuse(
                "completion retry repair grant contradicts its publication",
            ));
        }
        let history = self.get_task_history(&task.id)?;
        let mut grants = Vec::new();
        for entry in history
            .iter()
            .filter(|entry| entry.event == "pr_auto_fix_out_of_run")
        {
            let note: Value = serde_json::from_str(entry.note.as_deref().unwrap_or_default())
                .map_err(|_| refuse("completion retry repair grant history unreadable"))?;
            if lineage
                .iter()
                .any(|run| note["resumed_run_id"] == run.run_id)
            {
                grants.push((entry, note));
            }
        }
        let [(record, note)] = grants.as_slice() else {
            return Err(refuse(
                "completion retry repair grant history missing or ambiguous",
            ));
        };
        let origin_id = field(note, "resumed_run_id")?;
        let origin_at = lineage
            .iter()
            .position(|run| run.run_id == origin_id)
            .ok_or_else(|| refuse("completion retry grant origin missing"))?;
        let origin = lineage[origin_at];
        if record.by != "system"
            || record.at < origin.created_at
            || Some(record.at) > origin.finished_at
            || origin_at == 0
            || origin.retry_source_run_id.as_deref() != Some(field(marker, "source_run_id")?)
            || [
                "source_run_id",
                "pr_number",
                "head_sha",
                "blocker",
                "drain_run_id",
            ]
            .iter()
            .any(|key| note[*key] != marker[*key])
            || lineage[origin_at..].iter().any(|run| {
                run.input.as_ref().and_then(|input| input.get(marker_key)) != Some(marker)
            })
        {
            return Err(refuse("completion retry repair grant ownership disagrees"));
        }
        if origin.run_id == retry.run_id {
            // rewind_to_completion clears every deadline. Completion may then
            // allocate only this PR's fresh deadline; no other budget is granted.
            if state.completion_deadlines.iter().any(|(number, deadline)| {
                number != pr
                    || i64::try_from(*deadline).map_or(true, |deadline| {
                        deadline <= origin.created_at.timestamp_millis()
                    })
            }) {
                return Err(refuse(
                    "completion retry renewed an unrelated or invalid deadline",
                ));
            }
        } else if state.completion_deadlines != parent_state.completion_deadlines {
            return Err(refuse(
                "completion retry changed an inherited repair deadline",
            ));
        }
        Ok(json!({"record":record,"origin":origin,"marker":marker}))
    }
}

fn is_delivery(job: &str) -> bool {
    matches!(job, "task_pr_pipeline" | "task_local_pipeline") || job.starts_with("task_claimed_")
}
