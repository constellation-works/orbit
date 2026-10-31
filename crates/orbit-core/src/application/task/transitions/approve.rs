use super::*;

impl OrbitRuntime {
    /// Move a task to backlog through the approval body when its current
    /// state is proposed, otherwise through the ordinary governed update.
    /// The outer lock keeps the dispatch decision and the selected transition
    /// body on one task snapshot.
    pub(crate) fn transition_task_to_backlog_with_identity(
        &self,
        id: &str,
        note: Option<String>,
        comment: Option<String>,
        agent: Option<String>,
        model: Option<String>,
    ) -> Result<Task, OrbitError> {
        let mut result = None;
        self.stores().tasks().with_task_write_lock(id, &mut || {
            let task = self.get_task(id)?;
            if task.status == TaskStatus::Proposed {
                ensure_status_change_not_run_owned(self, id)?;
            }
            result = Some(if task.status == TaskStatus::Proposed {
                self.approve_task_with_identity(
                    id,
                    note.clone(),
                    comment.clone(),
                    agent.clone(),
                    model.clone(),
                )?
            } else {
                if note.is_some() {
                    return Err(OrbitError::InvalidInput(
                        "`note` on a backlog transition is only valid when approving a proposed task"
                            .to_string(),
                    ));
                }
                self.update_task_with_identity(
                    id,
                    TaskUpdateParams {
                        comment: comment.clone(),
                        status: Some(TaskStatus::Backlog),
                        ..Default::default()
                    },
                    agent.clone(),
                    model.clone(),
                )?
            });
            Ok(())
        })?;
        result.ok_or_else(|| {
            OrbitError::Execution(
                "task backlog transition body did not run under the task lock".to_string(),
            )
        })
    }

    /// Trusted approval. Drain promotion calls this while its own run still
    /// lists the task, so this path does not apply the operator ownership
    /// guard. Operator surfaces use the guarded approval path.
    pub fn approve_task(
        &self,
        id: &str,
        note: Option<String>,
        comment: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.approve_task_with_identity(id, note, comment, None, None)
    }

    pub fn approve_task_with_identity(
        &self,
        id: &str,
        note: Option<String>,
        comment: Option<String>,
        agent: Option<String>,
        model: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.approve_task_locked(id, note, comment, agent, model, false)
    }

    /// Operator approval (`orbit task update --approve` and the dashboard).
    ///
    /// The ownership check runs under the task lock, before any comment or
    /// history write. A non-terminal local run whose `input.task_ids` lists
    /// the task refuses both `proposed → backlog` and `review → done`.
    pub(crate) fn approve_task_guarding_run_ownership(
        &self,
        id: &str,
        note: Option<String>,
        comment: Option<String>,
        agent: Option<String>,
        model: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.approve_task_locked(id, note, comment, agent, model, true)
    }

    fn approve_task_locked(
        &self,
        id: &str,
        note: Option<String>,
        comment: Option<String>,
        agent: Option<String>,
        model: Option<String>,
        guard_run_ownership: bool,
    ) -> Result<Task, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let note = note.map(|value| redact_all(&value));
        let comment = comment.map(|value| redact_all(&value));
        let (canonical_agent, canonical_model) =
            self.try_canonical_agent_model_identity(agent.as_deref(), model.as_deref())?;
        let actor = self.actor().clone();
        let effective_label = effective_actor_label(
            &actor.label,
            canonical_agent.as_deref(),
            canonical_model.as_deref(),
        )?;
        let append_comments = build_task_comments(comment, effective_label.as_str())?;
        let mut result = None;
        self.stores().tasks().with_task_write_lock(id, &mut || {
            let task = self.get_task(id)?;
            #[cfg(test)]
            self.apply_transition_read_hook(id)?;
            if guard_run_ownership
                && matches!(task.status, TaskStatus::Proposed | TaskStatus::Review)
            {
                ensure_status_change_not_run_owned(self, id)?;
            }
            let implemented_by =
                implementation_label(&task, effective_label.as_str(), canonical_model.as_deref());
            if task.status == TaskStatus::Review {
                super::super::lifecycle::ensure_post_deploy_criteria_met(self, &task)?;
                super::super::lifecycle::ensure_completion_run_stopped(self, &task, None, None)?;
                self.ensure_mutation_proposals_applied(&task)?;
                self.ensure_resolves_are_workspace_local(&task)?;
            }

            if task.status == TaskStatus::Proposed {
                self.ensure_finding_verified(&task)?;
                // Tool, operator and drain approvals land here, so the mock
                // check runs over the stored artifacts and tags. The desktop
                // edit path repeats the check in `desktop_task_write`.
                self.ensure_backlog_mock_admission(&task, None)?;
            }

            result = Some(match task.status {
                TaskStatus::Proposed => self.with_mutation(|| {
                    let task = self.stores().task_records().update(
                        id,
                        StoreTaskUpdateParams {
                            actor: effective_label.clone(),
                            status_event: Some("proposal_approved".to_string()),
                            status_note: note.clone(),
                            append_comments: append_comments.clone(),
                            expected_status: Some(vec![task.status]),
                            ..StoreTaskUpdateParams::from(TaskUpdateParams {
                                status: Some(TaskStatus::Backlog),
                                ..Default::default()
                            })
                        },
                    )?;
                    Ok((
                        task.clone(),
                        OrbitEvent::TaskProposalApproved {
                            id: id.to_string(),
                            approved_by: effective_label.clone(),
                        },
                    ))
                }),
                TaskStatus::Review => self.with_mutation(|| {
                    let task = self.stores().task_records().update(
                        id,
                        StoreTaskUpdateParams {
                            actor: effective_label.clone(),
                            status_event: Some("review_approved".to_string()),
                            status_note: note.clone(),
                            implemented_by: implemented_by.clone().map(Some),
                            append_comments: append_comments.clone(),
                            expected_status: Some(vec![task.status]),
                            ..StoreTaskUpdateParams::from(TaskUpdateParams {
                                status: Some(TaskStatus::Done),
                                ..Default::default()
                            })
                        },
                    )?;
                    Ok((
                        task.clone(),
                        OrbitEvent::TaskReviewApproved {
                            id: id.to_string(),
                            approved_by: effective_label.clone(),
                        },
                    ))
                }),
                other => Err(OrbitError::InvalidInput(format!(
                    "task '{id}' is in status '{other}'; approve requires 'proposed' or 'review'"
                ))),
            }?);
            Ok(())
        })?;
        let result = result.ok_or_else(|| {
            OrbitError::Execution("task approve body did not run under the task lock".to_string())
        })?;

        if result.status == TaskStatus::Done {
            self.record_resolves_side_effects(TaskStatus::Review, &result);
            // Approval to done only ever leaves `review`.
            self.close_task_prs_after_transition(TaskStatus::Review, &result, note.as_deref());
        }

        Ok(result)
    }

    /// Refuse a done transition whose unqualified `resolves` target lives in
    /// another workspace on this host (ORB-11078).
    ///
    /// Same-workspace targets and IDs that cannot be shown to belong
    /// elsewhere stay on the existing auto-resolve / dangling path.
    pub(crate) fn ensure_resolves_are_workspace_local(
        &self,
        task: &Task,
    ) -> Result<(), OrbitError> {
        let Ok(frictions) = crate::runtime::friction::store_for(self) else {
            return Ok(());
        };
        let workspace_id = self.workspace_id()?;
        ensure_resolves_targets_are_workspace_local(frictions.as_ref(), &workspace_id, task)
    }
}

/// Rejects an unqualified `resolves` friction ID that is missing locally
/// but present in another workspace on this host.
fn ensure_resolves_targets_are_workspace_local(
    frictions: &dyn FrictionStoreBackend,
    workspace_id: &str,
    task: &Task,
) -> Result<(), OrbitError> {
    for relation in &task.relations {
        if relation.relation_type != TaskRelationType::Resolves {
            continue;
        }
        let target = relation.target.as_str();
        if !is_valid_friction_id(target) {
            continue;
        }
        if frictions.show(target)?.is_some() {
            continue;
        }
        let found_in = frictions.foreign_owners_of(target)?;
        if !found_in.is_empty() {
            return Err(OrbitError::friction_not_local(
                target,
                task.id.clone(),
                workspace_id,
                found_in,
            ));
        }
    }
    Ok(())
}
