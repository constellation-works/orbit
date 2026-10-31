use super::*;

impl OrbitRuntime {
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
    /// explicitly restarted through `orbit.task.update`). Every other status is
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
        self.ensure_task_can_enter_workflow_honouring_schedule(id, workflow, true)
    }

    /// [`Self::ensure_task_can_enter_workflow_as_system`], skipping the
    /// scheduled hold when `honour_schedule` is false: only the explicit
    /// ship's operator override passes false, and it records the override
    /// before its run starts.
    pub(crate) fn ensure_task_can_enter_workflow_honouring_schedule(
        &self,
        id: &str,
        workflow: &str,
        honour_schedule: bool,
    ) -> Result<Task, OrbitError> {
        let task = self.get_task(id)?;
        if matches!(task.status, TaskStatus::Backlog | TaskStatus::InProgress) {
            match self.pilot_admission_hold(id)? {
                Some(super::super::PilotAdmissionHold::HostOperational(hold)) => {
                    if task.status == TaskStatus::Backlog {
                        self.record_host_operational_hold(id, &hold)?;
                    }
                    return Err(OrbitError::InvalidInput(hold.detail()));
                }
                Some(super::super::PilotAdmissionHold::OperatorValidation(hold)) => {
                    if task.status == TaskStatus::Backlog {
                        self.record_operator_validation_hold(id, &hold)?;
                    }
                    return Err(OrbitError::InvalidInput(hold.detail()));
                }
                Some(super::super::PilotAdmissionHold::UnverifiedFinding(hold)) => {
                    return Err(OrbitError::InvalidInput(hold.detail()));
                }
                // Only a backlog task is judged against this host: a claimed
                // `in-progress` one was admitted to its executor's OS.
                Some(super::super::PilotAdmissionHold::NativeOs(hold))
                    if task.status == TaskStatus::Backlog =>
                {
                    if let Some(wait) =
                        hold.wait_on(&task, self.host_os(), self.automation_machine_identity())
                    {
                        return Err(OrbitError::InvalidInput(wait));
                    }
                }
                _ => {}
            }
        }
        // A `backlog` task scheduled ahead is started by no dispatcher until
        // its time, unless an operator's `--ignore-schedule` override names
        // that time. This one check covers explicit ship, the gate's dispatch
        // re-check and worktree admission.
        if honour_schedule && let Some(hold) = self.schedule_hold(&task)? {
            return Err(hold.refusal(id));
        }
        if Self::workflow_admissible_statuses().contains(&task.status) {
            return Ok(task);
        }

        let next_step = match task.status {
            // Delivery already ran for these; re-shipping opens a second PR
            // or repeats landed work, so it must be a deliberate choice.
            TaskStatus::Review | TaskStatus::Done => {
                "Its delivery already ran; move it back to the backlog only if you mean to ship it again."
            }
            _ => {
                "Move it back to the backlog (or start it explicitly) before automation may run it."
            }
        };
        Err(OrbitError::InvalidInput(format!(
            "task '{id}' is in status '{}'; workflow admission for '{workflow}' requires 'backlog' or 'in-progress'. {next_step}",
            task.status
        )))
    }

    /// The single spelling of the admissible set, shared by the read-only gate
    /// and the compare-and-set the mutating admission writes with.
    pub(crate) fn workflow_admissible_statuses() -> [TaskStatus; 2] {
        [TaskStatus::Backlog, TaskStatus::InProgress]
    }

    /// Workflow admission: the status transition only. [ORB-12717] A crew is
    /// decided when the task is created (or when an operator clears the field),
    /// so admission reads `task.crew` and never writes it.
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
        // A `verify-mock-ui` task whose mocks no longer validate is refused
        // before it starts, on every local route that reaches this shared
        // transition (explicit ship, automatic discovery, local drains).
        let before = self.get_task(id)?;
        if let Err(detail) = self.mock_admission_check(&before) {
            return Err(OrbitError::InvalidInput(format!(
                "task '{id}' is not admitted for workflow '{workflow}': {detail}"
            )));
        }
        let mut admitted = None;
        self.stores().tasks().with_task_write_lock(id, &mut || {
            admitted = Some(self.admit_task_for_workflow_locked(id, workflow)?);
            Ok(())
        })?;
        let (admitted, transitioned) = admitted.ok_or_else(|| {
            OrbitError::Execution("workflow admission lock body did not run".into())
        })?;
        // Freeze the admitted mocks before implementation begins. A task
        // this call started gets a new delivery's baseline (kept when a
        // live ship run already froze this delivery's); an already
        // `in-progress` task keeps its active baseline.
        self.freeze_mock_digests_at_workflow_admission(&admitted, transitioned)?;
        Ok(admitted)
    }

    /// Admit under the per-task write lock. The bool reports whether this
    /// call performed the `backlog -> in-progress` transition (a new
    /// delivery) rather than finding the task already started.
    fn admit_task_for_workflow_locked(
        &self,
        id: &str,
        workflow: &str,
    ) -> Result<(Task, bool), OrbitError> {
        let task = self.ensure_task_can_enter_workflow_as_system(id, workflow)?;

        if task.status == TaskStatus::InProgress {
            return Ok((task, false));
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

        Ok((updated, true))
    }
}
