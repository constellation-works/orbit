use super::*;

/// Which lifecycle rules a status change on this write must satisfy
/// [ORB-12245].
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum StatusAuthority {
    /// In-process callers that own their own transition rules: the delivery
    /// pipeline's activities, which compare-and-set against the status they
    /// observed, and Core use cases such as [`OrbitRuntime::archive_task`].
    #[default]
    Internal,
    /// Attributed operator and agent surfaces — the CLI `task update`, the
    /// dashboard, and the registered `orbit.task.update` tool. The lifecycle
    /// table and local run ownership decide.
    Lifecycle,
    /// A human overriding the table or local run ownership from the bare CLI or the dashboard,
    /// recorded in task history as [`FORCED_STATUS_EVENT`].
    Forced,
}

#[derive(Default)]
pub(super) struct TaskUpdateContext {
    pub(super) status_note: Option<String>,
    pub(super) actor_override: Option<String>,
    pub(super) agent: Option<String>,
    pub(super) model: Option<String>,
    pub(super) artifact_owner: Option<String>,
    pub(super) expected_status: Option<TaskStatus>,
    pub(super) status_authority: StatusAuthority,
    pub(super) calling_run_id: Option<String>,
    /// Trusted entrypoint authority; task/tool input cannot populate it.
    pub(super) operator_decision_authority: bool,
    /// Orbit's own deterministic writer: artifacts record
    /// [`ArtifactWriter::System`]. Only `update_task_as_system` sets it.
    pub(super) system_writer: bool,
    /// A move into `in-progress` by a caller that is not a trusted operator
    /// starts work, so it is written as a `started` event and the store runs
    /// the execution-claim footprint guard, as the tool surface does. Only the
    /// attributed CLI entry point sets it.
    pub(super) guard_work_start: bool,
}

/// A locked write's result plus what the after-lock side effects need: the
/// status it replaced and the status note it recorded.
pub(super) struct LockedTaskUpdate {
    pub(super) task: Task,
    pub(super) previous_status: TaskStatus,
    pub(super) status_note: Option<String>,
}

pub(crate) struct ValidatedTaskFieldEdits {
    pub(crate) params: TaskUpdateParams,
    /// Set when this write assigned a crew explicitly or drew a replacement;
    /// the caller includes the draw source in the change history [ORB-12717].
    pub(crate) crew_assignment: Option<CreationCrewAssignment>,
    pub(crate) crew_source: Option<Option<String>>,
    pub(crate) crew_redraw_history: Option<TaskHistoryEntry>,
}

/// Who is editing an existing task's fields [ORB-16872].
///
/// Built from the same signals as `operator_write` in `update_task_locked`:
/// decision authority, canonical agent and model, the automation-actor label,
/// and whether this process declares an agent envelope. An attributed agent
/// or model is an agent writer even when that envelope is absent, which is
/// how a user-launched MCP server calls tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskFieldWriter {
    /// May add `skip-review` and `skip-validation`.
    Operator,
    /// Attributed agent or tool writer, or any writer inside an agent envelope.
    Agent,
    /// In-process machinery that is neither an operator nor an agent.
    Machinery,
}

/// Classify `writer` for shared field admission.
///
/// A non-empty canonical agent, or a model that names or infers an agent
/// family, is an agent writer. A bare label such as `human` is not: trusted
/// in-process callers store that label in the model slot without attributing
/// an agent.
pub(crate) fn task_field_writer(
    operator_decision_authority: bool,
    canonical_agent: Option<&str>,
    canonical_model: Option<&str>,
    actor_label: &str,
) -> TaskFieldWriter {
    if agent_context_declared() || attributed_agent_identity(canonical_agent, canonical_model) {
        TaskFieldWriter::Agent
    } else if operator_decision_authority
        && !super::super::helpers::is_automation_actor(actor_label)
    {
        TaskFieldWriter::Operator
    } else {
        TaskFieldWriter::Machinery
    }
}

/// Whether this identity is an agent family or a model string that infers one.
pub(super) fn attributed_agent_identity(agent: Option<&str>, model: Option<&str>) -> bool {
    let names_agent = |value: &str| {
        let value = value.trim();
        !value.is_empty()
            && (all_agent_families()
                .iter()
                .any(|family| value.eq_ignore_ascii_case(family))
                || infer_agent_family_from_model(value).is_some())
    };
    agent.is_some_and(names_agent) || model.is_some_and(names_agent)
}

impl OrbitRuntime {
    /// The in-crate task setter. Status changes are *not* checked against the
    /// lifecycle table here: callers are Core's own use cases, which either
    /// change no status or own the transition themselves. Everything outside
    /// this crate goes through a guarded entry point below.
    pub(crate) fn update_task(
        &self,
        id: &str,
        params: TaskUpdateParams,
    ) -> Result<Task, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.update_task_with_context(id, params, TaskUpdateContext::default())
    }

    pub fn update_task_with_identity(
        &self,
        id: &str,
        params: TaskUpdateParams,
        agent: Option<String>,
        model: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.update_task_with_context(
            id,
            params,
            TaskUpdateContext {
                operator_decision_authority: agent.is_none()
                    && model.is_none()
                    && self.actor().kind == crate::context::ActorKind::Human,
                agent,
                model,
                status_authority: StatusAuthority::Lifecycle,
                guard_work_start: true,
                ..Default::default()
            },
        )
    }

    /// Apply a dashboard-authored mutation under an explicit human label.
    /// Agent-facing callers use `update_task_with_identity`, whose provenance
    /// is validated as a canonical agent family.
    pub fn update_task_as_human(
        &self,
        id: &str,
        params: TaskUpdateParams,
        actor_label: String,
    ) -> Result<Task, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.update_task_with_context(
            id,
            params,
            TaskUpdateContext {
                actor_override: Some(actor_label),
                operator_decision_authority: true,
                status_authority: StatusAuthority::Lifecycle,
                ..Default::default()
            },
        )
    }

    /// The human escape hatch behind `orbit task update --force`: apply the
    /// update even when the lifecycle table or local run ownership refuses
    /// the status change, and record the override in task history.
    ///
    /// Only the two human surfaces reach this — the bare CLI and the
    /// dashboard's `PATCH /api/tasks/:id` with `force: true` (ORB-12445). The
    /// registered `orbit.task.update` tool refuses a `force` argument outright,
    /// so no agent can grant itself the override.
    pub fn force_update_task_with_identity(
        &self,
        id: &str,
        params: TaskUpdateParams,
        agent: Option<String>,
        model: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.update_task_with_context(
            id,
            params,
            TaskUpdateContext {
                operator_decision_authority: agent.is_none()
                    && model.is_none()
                    && self.actor().kind == crate::context::ActorKind::Human,
                agent,
                model,
                status_authority: StatusAuthority::Forced,
                ..Default::default()
            },
        )
    }

    /// Deterministic task machinery writes as the system, even when the
    /// runtime was opened by a human. Keep run ownership separate from the
    /// mutation's author; agent-facing writes use `update_task_with_owner`.
    pub(crate) fn update_task_as_system(
        &self,
        id: &str,
        params: TaskUpdateParams,
        owner: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.update_task_with_context(
            id,
            params,
            TaskUpdateContext {
                actor_override: Some(SYSTEM_ACTOR_LABEL.to_string()),
                artifact_owner: owner,
                status_authority: StatusAuthority::Lifecycle,
                system_writer: true,
                ..Default::default()
            },
        )
    }

    pub(crate) fn update_task_with_owner(
        &self,
        id: &str,
        params: TaskUpdateParams,
        agent: Option<String>,
        model: Option<String>,
        status_note: Option<String>,
        owner: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.update_task_with_context(
            id,
            params,
            TaskUpdateContext {
                agent,
                model,
                status_note,
                artifact_owner: owner,
                status_authority: StatusAuthority::Lifecycle,
                ..Default::default()
            },
        )
    }

    /// Apply an activity update on the coordination owner. Claimed workers
    /// must use the engine host's owner route; replica runtimes cannot write locally.
    pub fn update_task_from_activity(
        &self,
        id: &str,
        update: TaskActivityUpdate,
    ) -> Result<Task, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let TaskActivityUpdate {
            status,
            expected_status,
            execution_summary,
            comment,
            note,
            agent,
            model,
            calling_run_id,
        } = update;
        self.update_task_with_context(
            id,
            TaskUpdateParams {
                execution_summary,
                comment,
                status: Some(status),
                ..Default::default()
            },
            TaskUpdateContext {
                status_note: note,
                actor_override: Some(SYSTEM_ACTOR_LABEL.to_string()),
                agent,
                model,
                expected_status: Some(expected_status),
                calling_run_id,
                ..Default::default()
            },
        )
    }
}
