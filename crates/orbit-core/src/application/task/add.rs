use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_store::contracts::TaskCreateParams as StoreTaskCreateParams;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    Task, TaskStatus, TaskType, normalize_required_tools, normalize_task_dependencies,
    normalize_task_tags,
};

use crate::OrbitRuntime;

use super::helpers::{authored_role_value, build_task_comments, effective_actor_label};
use super::params::TaskAddParams;
use super::paths::{
    context_workspace_root, normalize_context_files_for_write, normalize_workspace_path,
};

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

    pub(crate) fn add_task_admitted(
        &self,
        mut params: TaskAddParams,
        agent: Option<String>,
        model: Option<String>,
        action_key: Option<&str>,
    ) -> Result<Task, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
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
        );
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
        let workspace_path =
            normalize_workspace_path(&self.paths().repo_root, params.workspace_path.as_deref())?;
        let dependencies = normalize_task_dependencies(params.dependencies.clone())?;
        self.validate_crew_name(params.crew.as_deref())?;
        params.orchestrator = self.canonical_crew_name(params.orchestrator.as_deref())?;
        if params.orchestrator.is_some()
            && !matches!(initial_status, TaskStatus::Proposed | TaskStatus::Backlog)
        {
            return Err(OrbitError::InvalidInput(format!(
                "initial status {initial_status} cannot carry an orchestrator; orchestrator can only be set while proposed or backlog"
            )));
        }

        let context_root =
            context_workspace_root(&self.paths().repo_root, workspace_path.as_deref());
        let context_files =
            normalize_context_files_for_write(params.context_files.clone(), &context_root)?;

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
                    workspace_path: workspace_path.clone(),
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
