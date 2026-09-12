use chrono::Utc;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::contracts::FrictionStoreBackend;
use orbit_types::identity::is_valid_friction_id;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    Task, TaskHistoryEntry, TaskRelationType, TaskStatus, unmet_task_dependencies,
};

use super::TaskRecordUpdateParams as StoreTaskUpdateParams;
use crate::OrbitRuntime;

use super::helpers::{
    SYSTEM_ACTOR_LABEL, build_task_comments, effective_actor_label, implementation_label,
};
use super::params::TaskUpdateParams;

#[cfg(test)]
use std::sync::Mutex;

const UNAUTHORED_TASK_PLAN_PLACEHOLDER: &str = "To be authored by executing agent at start time.";
const RELATION_RESOLVES: &str = "resolves";
/// [ORB-10470] Status event recorded when a resumed run restores its own
/// lineage's coupling to a task (re-admission and/or batch re-claim).
const RESUME_READMITTED_EVENT: &str = "resume_readmitted";

#[cfg(test)]
static TRANSITION_READ_HOOK_STATUS: Mutex<Option<(String, TaskStatus)>> = Mutex::new(None);

#[cfg(test)]
pub(super) static TRANSITION_READ_HOOK_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(super) fn set_transition_read_hook_status(id: Option<&str>, status: Option<TaskStatus>) {
    *TRANSITION_READ_HOOK_STATUS
        .lock()
        .expect("transition read hook mutex") =
        id.zip(status).map(|(id, status)| (id.to_string(), status));
}

#[derive(Debug, Default)]
struct StartTaskOptions {
    note: Option<String>,
    comment: Option<String>,
    agent: Option<String>,
    model: Option<String>,
    actor_label_override: Option<String>,
    crew_override: Option<String>,
}

impl OrbitRuntime {
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
        self.ensure_coordination_task_write_permitted()?;
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
            let implemented_by =
                implementation_label(&task, effective_label.as_str(), canonical_model.as_deref());
            if task.status == TaskStatus::Review {
                self.ensure_resolves_are_workspace_local(&task)?;
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
            self.record_resolves_side_effects(&result)?;
        }

        Ok(result)
    }

    pub(crate) fn record_resolves_side_effects(&self, task: &Task) -> Result<(), OrbitError> {
        for event in self.apply_resolves_side_effects(task) {
            self.record_event(event)?;
        }
        Ok(())
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

    pub(crate) fn apply_resolves_side_effects(&self, task: &Task) -> Vec<OrbitEvent> {
        let mut events = Vec::new();
        let frictions = match crate::runtime::friction::store_for(self) {
            Ok(store) => store,
            Err(error) => {
                // Without a store there is no per-relation verdict to give, so
                // report the failure once against each `resolves` target.
                return task
                    .relations
                    .iter()
                    .filter(|relation| relation.relation_type == TaskRelationType::Resolves)
                    .filter(|relation| is_valid_friction_id(&relation.target))
                    .map(|relation| OrbitEvent::TaskRelationSideEffectFailed {
                        task_id: task.id.clone(),
                        target: relation.target.clone(),
                        relation: RELATION_RESOLVES.to_string(),
                        reason: error.to_string(),
                    })
                    .collect();
            }
        };
        for relation in &task.relations {
            if relation.relation_type != TaskRelationType::Resolves {
                continue;
            }
            let target = relation.target.as_str();
            if !is_valid_friction_id(target) {
                continue;
            }
            match frictions.auto_resolve_by_task(target, &task.id, Utc::now()) {
                Ok(Some(_)) => events.push(OrbitEvent::FrictionAutoResolved {
                    task_id: task.id.clone(),
                    friction_id: target.to_string(),
                }),
                Ok(None) => events.push(OrbitEvent::TaskRelationDangling {
                    task_id: task.id.clone(),
                    target: target.to_string(),
                    relation: RELATION_RESOLVES.to_string(),
                }),
                Err(error) => events.push(OrbitEvent::TaskRelationSideEffectFailed {
                    task_id: task.id.clone(),
                    target: target.to_string(),
                    relation: RELATION_RESOLVES.to_string(),
                    reason: error.to_string(),
                }),
            }
        }
        events
    }

    pub fn start_task(
        &self,
        id: &str,
        note: Option<String>,
        comment: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.start_task_with_actor_label_override(
            id,
            StartTaskOptions {
                note,
                comment,
                ..Default::default()
            },
        )
    }

    pub fn start_task_with_identity_and_crew(
        &self,
        id: &str,
        note: Option<String>,
        comment: Option<String>,
        agent: Option<String>,
        model: Option<String>,
        crew_override: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.start_task_with_actor_label_override(
            id,
            StartTaskOptions {
                note,
                comment,
                agent,
                model,
                crew_override,
                ..Default::default()
            },
        )
    }

    pub(crate) fn start_task_as_system(
        &self,
        id: &str,
        note: Option<String>,
        comment: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.start_task_with_actor_label_override(
            id,
            StartTaskOptions {
                note,
                comment,
                actor_label_override: Some(SYSTEM_ACTOR_LABEL.to_string()),
                ..Default::default()
            },
        )
    }

    fn start_task_with_actor_label_override(
        &self,
        id: &str,
        options: StartTaskOptions,
    ) -> Result<Task, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let StartTaskOptions {
            note,
            comment,
            agent,
            model,
            actor_label_override,
            crew_override,
        } = options;
        let (canonical_agent, canonical_model) =
            self.try_canonical_agent_model_identity(agent.as_deref(), model.as_deref())?;
        let actor = self.actor().clone();
        let effective_label = match actor_label_override {
            Some(label) => label,
            None => effective_actor_label(
                &actor.label,
                canonical_agent.as_deref(),
                canonical_model.as_deref(),
            )?,
        };
        let append_comments = build_task_comments(comment, effective_label.as_str())?;
        let mut started = None;
        self.stores().tasks().with_task_write_lock(id, &mut || {
            let task = self.get_task(id)?;
            #[cfg(test)]
            self.apply_transition_read_hook(id)?;
            // Validate status before crew resolution so a misleading
            // "no crew selected" error can't mask the real problem
            // (e.g. trying to restart a task that's already in-progress).
            match task.status {
                TaskStatus::Proposed
                | TaskStatus::Backlog
                | TaskStatus::Someday
                | TaskStatus::Blocked => {}
                TaskStatus::InProgress => {
                    return Err(OrbitError::InvalidInput(format!(
                        "task '{id}' is already in-progress"
                    )));
                }
                other => {
                    return Err(OrbitError::InvalidInput(format!(
                        "task '{id}' is in status '{other}'; start requires 'proposed', 'backlog', 'someday', or 'blocked'"
                    )));
                }
            }
            self.resolve_and_log_crew_for_task_start(
                id,
                crew_override.as_deref(),
                task.crew.as_deref(),
            )?;
            let dependency_status_index = self.task_status_index()?;
            let unmet_dependencies = unmet_task_dependencies(&task, &dependency_status_index);
            if in_progress_transition_requires_plan(task.status) {
                ensure_task_has_execution_plan(id, task.plan.as_str())?;
            }
            let unmet_dependency_labels: Vec<String> = unmet_dependencies
                .iter()
                .map(|dependency| dependency.label())
                .collect();
            let warn_unmet_dependencies = || {
                if !unmet_dependency_labels.is_empty() {
                    orbit_common::tracing::warn!(
                        target: "orbit.task.dependencies",
                        task_id = id,
                        unmet = unmet_dependency_labels.join(",").as_str(),
                        "task has unmet dependencies",
                    );
                }
            };

            started = Some(match task.status {
                TaskStatus::Proposed => {
                warn_unmet_dependencies();
                let result = self.with_mutation(|| {
                    let at = chrono::Utc::now();
                    let task = self.stores().task_records().update(
                        id,
                        StoreTaskUpdateParams {
                            actor: effective_label.clone(),
                            status_event: Some("started".to_string()),
                            append_history: vec![TaskHistoryEntry {
                                at,
                                by: effective_label.clone(),
                                event: "proposal_approved".to_string(),
                                note: note.clone(),
                                from_status: Some(task.status),
                                to_status: Some(TaskStatus::Backlog),
                            }],
                            append_comments: append_comments.clone(),
                            expected_status: Some(vec![task.status]),
                            ..StoreTaskUpdateParams::from(TaskUpdateParams {
                                status: Some(TaskStatus::InProgress),
                                ..Default::default()
                            })
                        },
                    )?;
                    Ok((
                        task.clone(),
                        OrbitEvent::TaskStarted {
                            id: id.to_string(),
                            started_by: effective_label.clone(),
                            approved_from_proposed: true,
                        },
                    ))
                })?;
                    Ok(result)
                }
                TaskStatus::Backlog | TaskStatus::Someday | TaskStatus::Blocked => {
                warn_unmet_dependencies();
                let task = self.with_mutation(|| {
                    let task = self.stores().task_records().update(
                        id,
                        StoreTaskUpdateParams {
                            actor: effective_label.clone(),
                            status_event: Some("started".to_string()),
                            status_note: note.clone(),
                            append_comments: append_comments.clone(),
                            expected_status: Some(vec![task.status]),
                            ..StoreTaskUpdateParams::from(TaskUpdateParams {
                                status: Some(TaskStatus::InProgress),
                                ..Default::default()
                            })
                        },
                    )?;
                    Ok((
                        task.clone(),
                        OrbitEvent::TaskStarted {
                            id: id.to_string(),
                            started_by: effective_label.clone(),
                            approved_from_proposed: false,
                        },
                    ))
                })?;
                    Ok(task)
                }
                TaskStatus::InProgress => Err(OrbitError::InvalidInput(format!(
                    "task '{id}' is already in-progress"
                ))),
                other => Err(OrbitError::InvalidInput(format!(
                    "task '{id}' is in status '{other}'; start requires 'proposed', 'backlog', 'someday', or 'blocked'"
                ))),
            }?);
            Ok(())
        })?;
        started.ok_or_else(|| {
            OrbitError::Execution("task start body did not run under the task lock".to_string())
        })
    }

    /// Lifecycle half of workflow admission: is this task's *status* one a
    /// workflow may start from?
    ///
    /// It deliberately does not answer whether the task's done dependencies
    /// have been delivered into the base the run will be cut from — that
    /// question needs the effective base, which only the worktree step knows,
    /// and is answered there before the worktree is created (ORB-10464,
    /// `orbit-engine`'s `vcs::worktree::dependency_delivery`). Both halves
    /// must hold for a task to be genuinely ready.
    ///
    /// [ORB-11305] The set is exactly `backlog` (fresh authorized work) and
    /// `in-progress` (this run's own idempotent retry, or work a human
    /// explicitly restarted through `orbit.task.start`). Every other status is
    /// somebody's decision that this task should not be running right now, and
    /// automation must not overturn it:
    ///
    /// - `proposed` / `someday` — not approved, or withdrawn from the backlog.
    /// - `archived` / `rejected` — a human closed it.
    /// - `review` / `done` — the work already landed.
    /// - `blocked` — a run failed on it and a human has not looked yet.
    ///
    /// This matters most for queued work: a gate that was admitted while the
    /// task was `backlog` can sit in `wait_for_window` for the better part of
    /// an hour, and the dispatch decision it made back then is a snapshot, not
    /// standing approval. Callers re-ask this question at the dispatch and
    /// start boundaries so a withdrawal that lands during the wait wins.
    pub(crate) fn ensure_task_can_enter_workflow_as_system(
        &self,
        id: &str,
        workflow: &str,
    ) -> Result<Task, OrbitError> {
        let task = self.get_task(id)?;
        if Self::workflow_admissible_statuses().contains(&task.status) {
            return Ok(task);
        }

        Err(OrbitError::InvalidInput(format!(
            "task '{id}' is in status '{}'; workflow admission for '{workflow}' requires 'backlog' or 'in-progress'. \
             Move it back to the backlog (or start it explicitly) before automation may run it.",
            task.status
        )))
    }

    /// The single spelling of the admissible set, shared by the read-only gate
    /// and the compare-and-set the mutating admission writes with.
    fn workflow_admissible_statuses() -> [TaskStatus; 2] {
        [TaskStatus::Backlog, TaskStatus::InProgress]
    }

    pub(crate) fn admit_task_for_workflow_as_system(
        &self,
        id: &str,
        workflow: &str,
    ) -> Result<Task, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let workflow = workflow.trim();
        let workflow = if workflow.is_empty() {
            "workflow"
        } else {
            workflow
        };
        let task = self.ensure_task_can_enter_workflow_as_system(id, workflow)?;

        if task.status == TaskStatus::InProgress {
            return Ok(task);
        }

        let note = Some(format!("workflow admission: {workflow}"));
        // [ORB-11305] The predicate above read the status; this write re-checks
        // it under the store's per-task lock. Without the compare-and-set a
        // withdrawal landing in that gap would be overwritten by a `backlog`
        // snapshot taken before it — exactly how an archived task was restarted.
        let updated = self.with_mutation(|| {
            let task = self.stores().task_records().update(
                id,
                StoreTaskUpdateParams {
                    actor: SYSTEM_ACTOR_LABEL.to_string(),
                    status_event: Some("started".to_string()),
                    status_note: note.clone(),
                    expected_status: Some(Self::workflow_admissible_statuses().to_vec()),
                    ..StoreTaskUpdateParams::from(TaskUpdateParams {
                        status: Some(TaskStatus::InProgress),
                        ..Default::default()
                    })
                },
            )?;
            Ok((
                task.clone(),
                OrbitEvent::TaskStarted {
                    id: id.to_string(),
                    started_by: SYSTEM_ACTOR_LABEL.to_string(),
                    approved_from_proposed: false,
                },
            ))
        })?;

        Ok(updated)
    }

    /// [ORB-10470] Restore a task's coupling to a resumed run's lineage.
    ///
    /// Two repairs, applied as one write so the task's history records a single
    /// reconciliation:
    ///
    /// - `blocked` → `in-progress`, undoing the block that the source run's own
    ///   failure applied. This is a *restoration*, not a fresh admission: the
    ///   lineage already admitted this task (it was `in-progress` under the
    ///   source run), so the plan guard that gates a cold `blocked` → started
    ///   transition does not apply here.
    /// - `job_run_id` → the batch id the resumed checkpoints keep using, so the
    ///   delivery tail's ownership check (`load_handoff_context`) sees the same
    ///   identity the reused `worktree_setup` output carries.
    ///
    /// Callers must have already proven lineage ownership
    /// (`reconcile_resume_task_ownership`); this function does not re-derive
    /// it. Returns `None` when nothing needed changing, which makes a repeated
    /// resume of the same source a no-op.
    pub(crate) fn reclaim_task_for_resumed_run(
        &self,
        id: &str,
        batch_run_id: Option<&str>,
        source_run_id: &str,
        resumed_run_id: &str,
    ) -> Result<Option<Task>, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let task = self.get_task(id)?;
        let readmit = task.status == TaskStatus::Blocked;
        let restamp = batch_run_id
            .is_some_and(|batch_run_id| task.job_run_id.as_deref() != Some(batch_run_id));
        if !readmit && !restamp {
            return Ok(None);
        }

        let note = Some(format!(
            "resume lineage reconciliation: run '{resumed_run_id}' resumes '{source_run_id}'"
        ));
        let event = if readmit {
            OrbitEvent::TaskStarted {
                id: id.to_string(),
                started_by: SYSTEM_ACTOR_LABEL.to_string(),
                approved_from_proposed: false,
            }
        } else {
            OrbitEvent::TaskUpdated { id: id.to_string() }
        };
        let updated = self.with_mutation(|| {
            let task = self.stores().task_records().update(
                id,
                StoreTaskUpdateParams {
                    actor: SYSTEM_ACTOR_LABEL.to_string(),
                    status_event: Some(RESUME_READMITTED_EVENT.to_string()),
                    status_note: note.clone(),
                    ..StoreTaskUpdateParams::from(TaskUpdateParams {
                        status: readmit.then_some(TaskStatus::InProgress),
                        job_run_id: restamp
                            .then(|| batch_run_id.map(ToOwned::to_owned))
                            .flatten()
                            .map(Some),
                        ..Default::default()
                    })
                },
            )?;
            Ok((task.clone(), event))
        })?;

        Ok(Some(updated))
    }

    pub fn reject_task(
        &self,
        id: &str,
        note: String,
        comment: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.reject_task_with_identity(id, note, comment, None, None)
    }

    pub fn reject_task_with_identity(
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
        let reason = reason.to_string();
        let append_comments = build_task_comments(comment, effective_label.as_str())?;

        let mut result = None;
        self.stores().tasks().with_task_write_lock(id, &mut || {
            let task = self.get_task(id)?;
            #[cfg(test)]
            self.apply_transition_read_hook(id)?;
            result = Some(match task.status {
                TaskStatus::Proposed => self.with_mutation(|| {
                let task = self.stores().task_records().update(
                    id,
                    StoreTaskUpdateParams {
                        actor: effective_label.clone(),
                        status: Some(TaskStatus::Rejected),
                        status_event: Some("proposal_rejected".to_string()),
                        status_note: Some(reason.clone()),
                        append_comments: append_comments.clone(),
                        expected_status: Some(vec![task.status]),
                        ..Default::default()
                    },
                )?;
                Ok((
                    task.clone(),
                    OrbitEvent::TaskProposalRejected {
                        id: id.to_string(),
                        rejected_by: effective_label.clone(),
                    },
                ))
                }),
                TaskStatus::Review => self.with_mutation(|| {
                let task = self.stores().task_records().update(
                    id,
                    StoreTaskUpdateParams {
                        actor: effective_label.clone(),
                        status: Some(TaskStatus::Rejected),
                        status_event: Some("review_rejected".to_string()),
                        status_note: Some(reason.clone()),
                        append_comments: append_comments.clone(),
                        expected_status: Some(vec![task.status]),
                        ..Default::default()
                    },
                )?;
                Ok((
                    task.clone(),
                    OrbitEvent::TaskReviewRejected {
                        id: id.to_string(),
                        rejected_by: effective_label.clone(),
                    },
                ))
                }),
                TaskStatus::Backlog => self.with_mutation(|| {
                let task = self.stores().task_records().update(
                    id,
                    StoreTaskUpdateParams {
                        actor: effective_label.clone(),
                        status: Some(TaskStatus::Rejected),
                        status_event: Some("backlog_rejected".to_string()),
                        status_note: Some(reason.clone()),
                        append_comments: append_comments.clone(),
                        expected_status: Some(vec![task.status]),
                        ..Default::default()
                    },
                )?;
                Ok((
                    task.clone(),
                    OrbitEvent::TaskProposalRejected {
                        id: id.to_string(),
                        rejected_by: effective_label.clone(),
                    },
                ))
                }),
                TaskStatus::InProgress => self.with_mutation(|| {
                let task = self.stores().task_records().update(
                    id,
                    StoreTaskUpdateParams {
                        actor: effective_label.clone(),
                        status: Some(TaskStatus::Rejected),
                        status_event: Some("in_progress_rejected".to_string()),
                        status_note: Some(reason.clone()),
                        append_comments: append_comments.clone(),
                        expected_status: Some(vec![task.status]),
                        ..Default::default()
                    },
                )?;
                Ok((
                    task.clone(),
                    OrbitEvent::TaskProposalRejected {
                        id: id.to_string(),
                        rejected_by: effective_label.clone(),
                    },
                ))
                }),
                other => Err(OrbitError::InvalidInput(format!(
                    "task '{id}' is in status '{other}'; reject requires 'proposed', 'review', 'backlog', or 'in-progress'"
                ))),
            }?);
            Ok(())
        })?;
        let result = result.ok_or_else(|| {
            OrbitError::Execution("task reject body did not run under the task lock".to_string())
        })?;

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
    fn apply_transition_read_hook(&self, id: &str) -> Result<(), OrbitError> {
        if let Some((hook_id, status)) = TRANSITION_READ_HOOK_STATUS
            .lock()
            .expect("transition read hook mutex")
            .clone()
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
        self.delete_task(id)
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

fn ensure_task_delete_allowed(id: &str, status: TaskStatus, force: bool) -> Result<(), OrbitError> {
    if force || matches!(status, TaskStatus::Proposed | TaskStatus::Rejected) {
        return Ok(());
    }

    Err(OrbitError::InvalidInput(format!(
        "task '{id}' is in status '{status}'; use --force to delete tasks not in proposed or rejected status"
    )))
}

pub(crate) fn ensure_task_has_execution_plan(id: &str, plan: &str) -> Result<(), OrbitError> {
    let normalized = plan.trim();
    if normalized.is_empty() || normalized == UNAUTHORED_TASK_PLAN_PLACEHOLDER {
        return Err(OrbitError::InvalidInput(format!(
            "task '{id}' requires a non-empty execution plan before transitioning to in-progress"
        )));
    }
    Ok(())
}

pub(crate) fn in_progress_transition_requires_plan(from_status: TaskStatus) -> bool {
    !matches!(from_status, TaskStatus::Backlog | TaskStatus::InProgress)
}
