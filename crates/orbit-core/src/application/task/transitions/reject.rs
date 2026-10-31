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
        let comment = comment.map(|value| redact_all(&value));
        let append_comments = build_task_comments(comment, effective_label.as_str())?;

        let mut result = None;
        let mut previous_status = None;
        self.stores().tasks().with_task_write_lock(id, &mut || {
            let task = self.get_task(id)?;
            #[cfg(test)]
            self.apply_transition_read_hook(id)?;
            previous_status = Some(task.status);
            // Persisted status event and emitted event per rejectable status.
            // `review_rejected` selects `TaskReviewRejected`; the others emit
            // `TaskProposalRejected`.
            let (status_event, review_rejected) = match task.status {
                TaskStatus::Proposed => ("proposal_rejected", false),
                TaskStatus::Review => ("review_rejected", true),
                TaskStatus::Backlog => ("backlog_rejected", false),
                TaskStatus::InProgress => ("in_progress_rejected", false),
                other => {
                    return Err(OrbitError::InvalidInput(format!(
                        "task '{id}' is in status '{other}'; reject requires 'proposed', 'review', 'backlog', or 'in-progress'"
                    )));
                }
            };
            result = Some(self.with_mutation(|| {
                let task = self.stores().task_records().update(
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
                )?;
                let event = if review_rejected {
                    OrbitEvent::TaskReviewRejected {
                        id: id.to_string(),
                        rejected_by: effective_label.clone(),
                    }
                } else {
                    OrbitEvent::TaskProposalRejected {
                        id: id.to_string(),
                        rejected_by: effective_label.clone(),
                    }
                };
                Ok((task.clone(), event))
            })?);
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

    pub fn delete_task(&self, id: &str) -> Result<(), OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.with_mutation(|| {
            let deleted = self.stores().task_records().delete(id)?;
            if !deleted {
                return Err(OrbitError::not_found(NotFoundKind::Task, id.to_string()));
            }
            Ok(((), OrbitEvent::TaskDeleted { id: id.to_string() }))
        })
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

    Err(OrbitError::InvalidInput(format!(
        "task '{id}' is in status '{status}'; use --force to delete tasks not in proposed or rejected status"
    )))
}
