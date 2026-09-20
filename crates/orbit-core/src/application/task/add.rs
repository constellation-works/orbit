use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_store::contracts::TaskCreateParams as StoreTaskCreateParams;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    Task, TaskStatus, TaskType, normalize_required_tools, normalize_task_dependencies,
    normalize_task_tags,
};
use sha2::{Digest, Sha256};

use crate::OrbitRuntime;
use crate::application::job::crew_pools::random_crew_ticket;

use super::TaskRecordUpdateParams as StoreTaskUpdateParams;
use super::helpers::{
    SYSTEM_ACTOR_LABEL, authored_role_value, build_task_comments, crew_assigned_history,
    effective_actor_label,
};
use super::params::TaskAddParams;
use super::paths::normalize_context_files_for_write;

const AUTO_TASK_TITLE_PREFIX: &str = "[auto-task] ";

/// Task-finding provenance in canonical precedence order. When several tags
/// are present, the first mapping wins regardless of caller-provided tag order.
const TASK_PROVENANCE_TITLE_PREFIXES: &[(&str, &str)] = &[
    ("qa-sweep", "[qa-sweep] "),
    ("security-review", "[security-review] "),
    ("code-review", "[code-review] "),
    ("friction-curation", "[friction-curation] "),
];

impl OrbitRuntime {
    /// Validate task-scoped tool requirements against the current registry.
    ///
    /// A requirement is durable metadata, so a name an operator has disabled is
    /// kept: `orbit tool enable` restores it, so that state is only a warning.
    /// An unknown name, and a registered tool that is not on the agent surface
    /// at all, are both rejected — activity admission refuses them and
    /// `required_tools` is immutable after creation, so the record would be
    /// impossible to dispatch and impossible to repair.
    pub fn validate_required_tools(
        &self,
        required_tools: &[String],
    ) -> Result<Vec<String>, OrbitError> {
        let mut warnings = Vec::new();
        for name in normalize_required_tools(required_tools.to_vec()) {
            if !self.tool_registry().has(&name) {
                return Err(self.ungrantable_required_tool(format!(
                    "required_tools contains unregistered tool '{name}'"
                )));
            }
            if !self.tool_registry().is_active(&name) {
                return Err(self.ungrantable_required_tool(format!(
                    "required_tools contains tool '{name}', which is an admin/human-only \
                     operation that is never granted to an agent"
                )));
            }

            let stored_disabled = self
                .stores()
                .tools()
                .get_tool(&name)?
                .is_some_and(|tool| !tool.enabled);
            if stored_disabled {
                warnings.push(format!(
                    "required_tools includes registered tool '{name}', which is currently disabled"
                ));
            }
        }

        Ok(warnings)
    }

    /// Reject one requirement an agent could never be granted, suggesting the
    /// agent-facing tool names instead.
    fn ungrantable_required_tool(&self, message: String) -> OrbitError {
        let mut agent_facing_names = self
            .tool_registry()
            .schemas()
            .into_iter()
            .map(|schema| schema.name)
            .collect::<Vec<_>>();
        agent_facing_names.sort();

        OrbitError::invalid_input_with_suggestions(message, agent_facing_names)
    }

    pub fn add_task(&self, params: TaskAddParams) -> Result<Task, OrbitError> {
        self.add_task_with_identity(params, None, None)
    }

    pub fn add_task_with_identity(
        &self,
        params: TaskAddParams,
        agent: Option<String>,
        model: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.add_task_admitted(params, agent, model, None)
    }

    /// The record shape every task had before [ORB-12717] moved crew
    /// assignment into creation: `crew` genuinely unset. Dispatch still routes
    /// such a task through the complexity pools, and the tests that cover that
    /// fallback need a record `add_task` no longer produces.
    #[cfg(test)]
    pub(crate) fn add_crew_less_task_for_tests(
        &self,
        params: TaskAddParams,
    ) -> Result<Task, OrbitError> {
        let task = self.add_task(params)?;
        self.stores().task_records().update(
            &task.id,
            StoreTaskUpdateParams {
                actor: SYSTEM_ACTOR_LABEL.to_string(),
                crew: Some(None),
                ..Default::default()
            },
        )
    }

    pub(crate) fn add_task_admitted(
        &self,
        mut params: TaskAddParams,
        agent: Option<String>,
        model: Option<String>,
        action_key: Option<&str>,
    ) -> Result<Task, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.validate_required_tools(&params.required_tools)?;

        // [ORB-00417] Redact secrets at the single task-creation choke point
        // (shared by the dashboard POST, CLI `task add`, and the MCP task tool)
        // so a pasted key never lands in the task registry or the audit trail.
        // `redact_all` is idempotent, so read-time redaction still composes.
        params.title = redact_all(&params.title);
        params.description = redact_all(&params.description);
        params.plan = redact_all(&params.plan);
        for criterion in params.acceptance_criteria.iter_mut() {
            *criterion = redact_all(criterion);
        }
        params.comment = params.comment.map(|comment| redact_all(&comment));

        let normalized_tags = normalize_task_tags(params.tags.clone());
        params.title = title_with_provenance_prefix(&params.title, &normalized_tags);

        let (canonical_agent, canonical_model) =
            self.try_canonical_agent_model_identity(agent.as_deref(), model.as_deref())?;
        let actor = self.actor().clone();
        let effective_label = effective_actor_label(
            &actor.label,
            canonical_agent.as_deref(),
            canonical_model.as_deref(),
        )?;
        let (task_type, initial_status) = infer_task_create_type_and_status(
            params.task_type,
            params.status,
            TaskStatus::Proposed,
        )?;
        let uses_system_identity = params.system_created;
        let create_label = if uses_system_identity {
            "system".to_string()
        } else {
            effective_label.clone()
        };
        let planned_by = authored_role_value(params.plan.as_str(), &create_label);
        let comments = build_task_comments(params.comment.clone(), create_label.as_str())?;
        let dependencies = normalize_task_dependencies(params.dependencies.clone())?;
        self.validate_crew_name(params.crew.as_deref())?;
        // [ORB-12717] Crew is decided once, here, and never by a later status
        // transition. A caller-supplied crew is kept verbatim; otherwise the
        // complexity pools (then `default_crew`) choose one for this task.
        let crew_assignment = self.creation_crew_assignment(
            Some(params.complexity),
            params.crew.as_deref(),
            &mut creation_crew_ticket(action_key),
        )?;
        params.crew = crew_assignment
            .as_ref()
            .map(|assignment| assignment.crew.clone());
        params.orchestrator = self.canonical_crew_name(params.orchestrator.as_deref())?;
        if params.orchestrator.is_some()
            && !matches!(initial_status, TaskStatus::Proposed | TaskStatus::Backlog)
        {
            return Err(OrbitError::InvalidInput(format!(
                "initial status {initial_status} cannot carry an orchestrator; orchestrator can only be set while proposed or backlog"
            )));
        }

        // Context selectors are stored relative to the repository root. The
        // former `--workspace-path` hint changed validation roots without
        // being persisted, leaving every reader with a different root.
        let context_files = normalize_context_files_for_write(
            params.context_files.clone(),
            &self.paths().repo_root,
        )?;

        let task = self.with_mutation(|| {
            let task = self.stores().task_records().create_with_key(
                StoreTaskCreateParams {
                    actor: create_label.clone(),
                    parent_id: params.parent_id.clone(),
                    title: params.title.clone(),
                    description: params.description.clone(),
                    acceptance_criteria: params.acceptance_criteria.clone(),
                    dependencies: dependencies.clone(),
                    relations: params.relations.clone(),
                    tags: normalized_tags.clone(),
                    required_tools: normalize_required_tools(params.required_tools.clone()),
                    plan: params.plan.clone(),
                    execution_summary: String::new(),
                    context_files,
                    repo_root: None,
                    created_by: Some(create_label.clone()),
                    planned_by,
                    implemented_by: None,
                    status: initial_status,
                    priority: params.priority,
                    complexity: Some(params.complexity),
                    task_type,
                    external_refs: params.external_refs.clone(),
                    source_task_id: params.source_task_id.clone(),
                    crew: params.crew.clone(),
                    orchestrator: params.orchestrator.clone(),
                    comments: comments.clone(),
                },
                action_key,
            )?;
            // The create contract carries no history, so the provenance entry
            // is a second write inside the same mutation as the insert.
            let task = match &crew_assignment {
                Some(assignment) => self.stores().task_records().update(
                    &task.id,
                    StoreTaskUpdateParams {
                        actor: SYSTEM_ACTOR_LABEL.to_string(),
                        append_history: vec![crew_assigned_history(assignment)],
                        ..Default::default()
                    },
                )?,
                None => task,
            };
            Ok((
                task.clone(),
                OrbitEvent::TaskAdded {
                    id: task.id.clone(),
                },
            ))
        })?;

        Ok(task)
    }
}

/// Tickets for the creation-time crew draw.
///
/// An idempotent create (an auto-task mint) reserves its task id against a
/// digest of these very parameters, so a retried mint that drew a different
/// crew would be refused as a changed input. A keyed create therefore draws
/// from the action key, which makes the retry reproduce the first draw; an
/// ordinary create draws from the system RNG.
fn creation_crew_ticket(
    action_key: Option<&str>,
) -> impl FnMut() -> Result<u64, OrbitError> + use<> {
    let action_key = action_key.map(ToOwned::to_owned);
    let mut round: u64 = 0;
    move || match &action_key {
        Some(key) => {
            let digest = Sha256::digest(format!("crew:{key}:{round}").as_bytes());
            round += 1;
            let mut ticket = [0u8; 8];
            ticket.copy_from_slice(&digest[..8]);
            Ok(u64::from_be_bytes(ticket))
        }
        None => random_crew_ticket(),
    }
}

/// Apply visible provenance at the shared task-creation boundary used by CLI,
/// MCP, dashboard, scheduler, and internal callers. Auto-task provenance wins
/// so scheduler-minted parent tasks keep their established title convention;
/// finding provenance otherwise follows the fixed table above.
fn title_with_provenance_prefix(title: &str, tags: &[String]) -> String {
    let prefix = if tags.iter().any(|tag| tag.starts_with("auto-task:")) {
        Some(AUTO_TASK_TITLE_PREFIX)
    } else {
        TASK_PROVENANCE_TITLE_PREFIXES
            .iter()
            .find_map(|(tag, prefix)| {
                tags.iter()
                    .any(|candidate| candidate == tag)
                    .then_some(*prefix)
            })
    };

    match prefix {
        Some(prefix) if !title.starts_with(prefix) => format!("{prefix}{title}"),
        _ => title.to_string(),
    }
}

fn infer_task_create_type_and_status(
    requested_type: Option<TaskType>,
    requested_status: Option<TaskStatus>,
    default_status: TaskStatus,
) -> Result<(TaskType, TaskStatus), OrbitError> {
    if requested_status == Some(TaskStatus::Archived) {
        return Err(OrbitError::InvalidInput(
            "status 'archived' cannot be set at task creation; use the archive command".to_string(),
        ));
    }

    Ok((
        requested_type.unwrap_or(TaskType::Chore),
        requested_status.unwrap_or(default_status),
    ))
}
