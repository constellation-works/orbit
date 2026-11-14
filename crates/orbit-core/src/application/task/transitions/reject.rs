use super::*;

impl OrbitRuntime {
    pub fn reject_task(
        &self,
        id: &str,
        note: String,
        comment: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.reject_task_with_identity(id, note, comment, None, None)
    }

    pub(crate) fn reject_task_with_identity(
        &self,
        id: &str,
        note: String,
        comment: Option<String>,
        agent: Option<String>,
        model: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let (canonical_agent, canonical_model) =
            self.try_canonical_agent_model_identity(agent.as_deref(), model.as_deref())?;
        let actor = self.actor().clone();
        let effective_label = effective_actor_label(
            &actor.label,
            canonical_agent.as_deref(),
            canonical_model.as_deref(),
        )?;
        let reason = note.trim();
        if reason.is_empty() {
            return Err(OrbitError::InvalidInput(
                "rejection note must not be empty".to_string(),
            ));
        }
        let reason = redact_all(reason);
        if comment
            .as_deref()
            .is_some_and(orbit_types::workflow::ResidentComment::is_resident_comment)
        {
            return Err(OrbitError::InvalidInput("append resident records using a comment-only task update, separately from lifecycle transitions".into()));
        }
        let comment = comment.map(|value| redact_all(&value));
        let append_comments = build_task_comments(comment, effective_label.as_str())?;

        let mut result = None;
        let mut previous_status = None;
        self.stores().tasks().with_task_write_lock(id, &mut || {
            let task = self.get_task(id)?;
            #[cfg(test)]
            self.apply_transition_read_hook(id)?;
            previous_status = Some(task.status);
            let status_event = match task.status {
                TaskStatus::Proposed => "proposal_rejected",
                TaskStatus::Review => "review_rejected",
                TaskStatus::Backlog => "backlog_rejected",
                TaskStatus::InProgress => "in_progress_rejected",
                other => {
                    return Err(OrbitError::InvalidInput(format!(
                        "task '{id}' is in status '{other}'; reject requires 'proposed', 'review', 'backlog', or 'in-progress'"
                    )));
                }
            };
            result = Some(self.stores().task_records().update(
                id,
                StoreTaskUpdateParams {
                    actor: effective_label.clone(),
                    status: Some(TaskStatus::Rejected),
                    status_event: Some(status_event.to_string()),
                    status_note: Some(reason.clone()),
                    append_comments: append_comments.clone(),
                    expected_status: Some(vec![task.status]),
                    ..Default::default()
                },
            )?);
            Ok(())
        })?;
        let result = result.ok_or_else(|| {
            OrbitError::Execution("task reject body did not run under the task lock".to_string())
        })?;
        if let Some(previous_status) = previous_status {
            self.close_task_prs_after_transition(previous_status, &result, Some(&reason));
        }

        Ok(result)
    }

    pub fn archive_task(&self, id: &str) -> Result<(), OrbitError> {
        self.update_task(
            id,
            TaskUpdateParams {
                status: Some(TaskStatus::Archived),
                ..Default::default()
            },
        )
        .map(|_| ())
    }

    /// Archive as the dashboard's archive button does: refused while a
    /// local run owns the task unless `force`, in which case the move is
    /// applied under the forced authority and recorded as `forced`. The
    /// plain [`Self::archive_task`] stays the internal path its in-process
    /// callers own.
    pub fn archive_task_for_dashboard(&self, id: &str, force: bool) -> Result<(), OrbitError> {
        if force {
            return self
                .force_update_task_with_identity(
                    id,
                    TaskUpdateParams {
                        status: Some(TaskStatus::Archived),
                        ..Default::default()
                    },
                    None,
                    None,
                )
                .map(|_| ());
        }
        ensure_status_change_not_run_owned(self, id)?;
        // The preflight can go stale while a local run starts owning the
        // task. Re-read ownership under the task write lock and hold it
        // through the archive write, like `delete_task_guarded` does.
        self.stores().tasks().with_task_write_lock(id, &mut || {
            #[cfg(test)]
            self.apply_transition_read_hook(id)?;
            #[cfg(any(test, feature = "test-support"))]
            self.apply_transition_run_hook(id)?;
            ensure_status_change_not_run_owned(self, id)?;
            self.archive_task(id)
        })
    }

    pub fn delete_task(&self, id: &str) -> Result<(), OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let deleted = self.stores().task_records().delete(id)?;
        if !deleted {
            return Err(OrbitError::not_found(NotFoundKind::Task, id.to_string()));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn apply_transition_read_hook(&self, id: &str) -> Result<(), OrbitError> {
        if let Some((hook_id, status)) = self.transition_read_hook_status()
            && hook_id == id
        {
            self.update_task(
                id,
                TaskUpdateParams {
                    status: Some(status),
                    ..Default::default()
                },
            )?;
        }

        Ok(())
    }

    /// Test seam for the dashboard-archive ownership race: when the run hook
    /// names `id`, insert a non-terminal local run owning it. The archive
    /// path calls this after its outer pre-check and before its under-lock
    /// recheck, so the recheck observes the lost race.
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn apply_transition_run_hook(&self, id: &str) -> Result<(), OrbitError> {
        if let Some(hook_id) = self.transition_run_hook_task()
            && hook_id == id
        {
            self.stores().jobs().insert_job_run(
                "fixture",
                1,
                chrono::Utc::now(),
                Some(serde_json::json!({ "task_ids": [id] })),
                None,
            )?;
        }

        Ok(())
    }

    pub fn delete_task_guarded(&self, id: &str, force: bool) -> Result<(), OrbitError> {
        let task = self.get_task(id)?;
        ensure_task_delete_allowed(&task.id, task.status, force)?;
        #[cfg(test)]
        self.apply_transition_read_hook(id)?;
        // The preflight can go stale while a transition owns the task lock.
        // Re-read eligibility under that lock and hold it through deletion.
        self.stores().tasks().with_task_write_lock(id, &mut || {
            let task = self.get_task(id)?;
            ensure_task_delete_allowed(&task.id, task.status, force)?;
            self.delete_task(id)
        })
    }
}

fn ensure_task_delete_allowed(id: &str, status: TaskStatus, force: bool) -> Result<(), OrbitError> {
    if force || matches!(status, TaskStatus::Proposed | TaskStatus::Rejected) {
        return Ok(());
    }

    Err(OrbitError::force_overridable_delete(
        status.to_string(),
        format!(
            "task '{id}' is in status '{status}'; use --force to delete tasks not in proposed or rejected status"
        ),
    ))
}
