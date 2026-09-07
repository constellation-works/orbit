use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;

use chrono::Utc;
use orbit_common::fs::selector::canonical_selector;
use orbit_common::governance::friction::{FrictionVerb, effective_title, normalize_title};
use orbit_common::protocol::tool_input::{
    optional_csv_or_string_list_alias, optional_raw_string, optional_string, optional_string_alias,
    optional_string_list_alias, required_string,
};
use orbit_common::security::redaction::redact_all;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::Store;
use orbit_store::compose::{
    WorkspaceTaskBackends, coordination_task_backends, workspace_friction_store,
};
use orbit_store::contracts::{
    TaskArtifactUpdateParams, TaskCreateParams, TaskDocumentUpdateParams, TaskHistoryUpdateParams,
};
use orbit_store::friction_store::{
    FrictionAddParams, FrictionUpdateParams, prepare_hub_friction_root, readable_hub_friction_root,
};
use orbit_store::maintenance::task_registry::{
    BindWorkspaceParams, RegisterWorkspaceParams, TaskRegistryStore, task_registry_path,
};
use orbit_tools::{
    OrbitBuiltinAction, OrbitTaskScope, OrbitToolHost, ReservationOwnerContext, ToolContext,
    ToolRegistry,
};
use orbit_types::identity::{is_valid_friction_id, normalize_optional_attribution_label};
use orbit_types::record::FrictionStatus;
use orbit_types::task::{
    Task, TaskComment, TaskPriority, TaskStatus, TaskType, normalize_required_tools,
    normalize_task_dependencies, normalize_task_tags, resolve_task_dependencies,
    resolve_task_relations, task_show_record_field_json, unknown_task_show_field_message,
    validate_task_dependencies,
};
use orbit_types::tool::ToolSessionContext;
use serde_json::{Map, Value, json};

use crate::OrbitRuntime;
use crate::runtime::run_input::managed_run_context_run_id_from_env;

pub(crate) fn build_orbit_tool_host(
    runtime: &OrbitRuntime,
    task_id: Option<String>,
    run_id: Option<String>,
    session_context: ToolSessionContext,
) -> Arc<dyn OrbitToolHost> {
    Arc::new(RuntimeOrbitToolHost {
        runtime: runtime.clone(),
        task_scope: OrbitTaskScope {
            orbit_root: Some(runtime.data_root_path().to_path_buf()),
            task_id,
            run_id: run_id.or_else(trusted_env_run_id),
        },
        session_context,
    })
}

#[derive(Clone)]
struct RuntimeOrbitToolHost {
    runtime: OrbitRuntime,
    task_scope: OrbitTaskScope,
    /// The calling session's asserted grants, carried so a handler whose
    /// decision depends on *who is calling* can reach them [ORB-11354].
    ///
    /// The tool chokepoint resolves capabilities before dispatch, but its
    /// answer is a yes/no it does not pass on. `orbit.agent.invoke` needs the
    /// caller itself, because it records the authorizing operator on a durable
    /// admission and verifies the operation-specific identity and workspace
    /// scope of a remote caller the ordinary registry would otherwise allow.
    session_context: ToolSessionContext,
}

/// Checkout-independent executor for coordination-authoritative hub tools.
///
/// It deliberately has no `OrbitRuntime`, `WorkspacePaths`, local configuration,
/// owner store, or fabricated checkout. The stable logical workspace ID is the
/// sole task/friction partition key.
#[derive(Clone)]
pub struct HubCoordinationExecutor {
    inner: Arc<HubCoordinationState>,
}

struct HubCoordinationState {
    global_root: PathBuf,
    workspace_id: String,
    legacy_friction_root: Option<PathBuf>,
    tasks: WorkspaceTaskBackends,
    #[cfg(test)]
    update_hooks: CheckoutlessUpdateHooks,
}

#[cfg(test)]
pub(super) type CheckoutlessUpdateHook = Arc<dyn Fn(&Task, &Value) + Send + Sync>;

#[cfg(test)]
#[derive(Default)]
struct CheckoutlessUpdateHooks {
    after_pre_state_read: Mutex<Option<CheckoutlessUpdateHook>>,
    after_locked_state_read: Mutex<Option<CheckoutlessUpdateHook>>,
}

impl HubCoordinationExecutor {
    /// Registers the path-free task-registry partition for a logical workspace.
    /// Workspace initialization calls this once; identical repeats are safe.
    pub fn register_workspace(
        global_root: &Path,
        workspace_id: impl Into<String>,
        slug: impl Into<String>,
    ) -> Result<(), OrbitError> {
        let registry = TaskRegistryStore::open(&task_registry_path(global_root))?;
        registry.register_workspace(RegisterWorkspaceParams {
            workspace_id: workspace_id.into(),
            slug: slug.into(),
            repo_fingerprint: None,
        })?;
        Ok(())
    }

    /// Bind this checkout in the task registry. `--force` replaces an
    /// existing orbit-dir row so a synthetic parent(data-dir) bind cannot
    /// leave split-brain state after workspace init.
    pub fn bind_checkout(
        global_root: &Path,
        workspace_id: impl Into<String>,
        slug: impl Into<String>,
        repo_root: &Path,
        orbit_dir: &Path,
        replace_existing: bool,
    ) -> Result<(), OrbitError> {
        let registry = TaskRegistryStore::open(&task_registry_path(global_root))?;
        let params = BindWorkspaceParams {
            workspace_id: Some(workspace_id.into()),
            slug: slug.into(),
            repo_root: repo_root.to_path_buf(),
            workspace_path: repo_root.to_path_buf(),
            orbit_dir: orbit_dir.to_path_buf(),
            repo_fingerprint: None,
        };
        if replace_existing {
            registry.rebind_checkout(params)?;
        } else {
            registry.bind_workspace(params)?;
        }
        Ok(())
    }

    pub fn new(
        global_root: &Path,
        workspace_id: impl Into<String>,
        legacy_friction_root: Option<PathBuf>,
    ) -> Result<Self, OrbitError> {
        let workspace_id = workspace_id.into();
        let task_partition_id = workspace_id.clone();
        Self::new_with_task_partition(
            global_root,
            workspace_id,
            task_partition_id,
            legacy_friction_root,
        )
    }

    /// Same as [`HubCoordinationExecutor::new`], but pins the coordination task
    /// registry to an explicit partition key.
    ///
    /// `orbit workspace init` writes one ID to the host registry, the checkout
    /// identity, and the task registry, so the two keys normally coincide. For
    /// workspaces registered before that convergence they differ (L-0098), and
    /// the task registry only answers to the checkout-identity key. A caller
    /// that has resolved a validated local checkout passes that key here so
    /// coordination reads and writes land in the partition the checkout-local
    /// surfaces already use; `workspace_id` stays the logical ID that owns
    /// friction partitioning and audit identity.
    pub fn new_with_task_partition(
        global_root: &Path,
        workspace_id: impl Into<String>,
        task_partition_id: impl Into<String>,
        legacy_friction_root: Option<PathBuf>,
    ) -> Result<Self, OrbitError> {
        let registry = TaskRegistryStore::open(&task_registry_path(global_root))?;
        let tasks = coordination_task_backends(registry, task_partition_id.into());
        Ok(Self {
            inner: Arc::new(HubCoordinationState {
                global_root: global_root.to_path_buf(),
                workspace_id: workspace_id.into(),
                legacy_friction_root,
                tasks,
                #[cfg(test)]
                update_hooks: CheckoutlessUpdateHooks::default(),
            }),
        })
    }

    #[cfg(test)]
    pub(super) fn set_after_pre_state_read_hook(&self, hook: CheckoutlessUpdateHook) {
        *self
            .inner
            .update_hooks
            .after_pre_state_read
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(hook);
    }

    #[cfg(test)]
    pub(super) fn set_after_locked_state_read_hook(&self, hook: CheckoutlessUpdateHook) {
        *self
            .inner
            .update_hooks
            .after_locked_state_read
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(hook);
    }

    #[cfg(test)]
    fn update_hook(hook: &Mutex<Option<CheckoutlessUpdateHook>>, task: &Task, input: &Value) {
        let hook = hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook {
            hook(task, input);
        }
    }

    pub fn execute_tool(
        &self,
        name: &str,
        input: Value,
        session_context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let mut registry = ToolRegistry::new();
        registry.register_builtins();
        let context = ToolContext {
            cwd: std::env::current_dir()
                .ok()
                .map(|path| path.to_string_lossy().into_owned()),
            session_context,
            orbit_host: Some(Arc::new(self.clone())),
            ..Default::default()
        };
        registry.execute(name, &context, input)
    }

    fn actor(agent: Option<&str>, model: Option<&str>) -> String {
        normalize_optional_attribution_label(model.or(agent), model)
            .unwrap_or_else(|| "unknown".to_string())
    }

    fn task(&self, id: &str) -> Result<Task, OrbitError> {
        self.inner
            .tasks
            .task
            .get_task(id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, id.to_string()))
    }

    fn task_json(&self, task: &Task) -> Result<Value, OrbitError> {
        let status = self.inner.tasks.task.task_status_index()?;
        let mut value = super::json::task_to_json(task, &status);
        let object = value.as_object_mut().ok_or_else(|| {
            OrbitError::Execution("task JSON projection did not produce an object".to_string())
        })?;
        object.insert(
            "comments".to_string(),
            serde_json::to_value(
                self.inner
                    .tasks
                    .history
                    .get_task_comments(&task.id)?
                    .unwrap_or_default(),
            )
            .map_err(|error| OrbitError::Execution(format!("serialize comments: {error}")))?,
        );
        object.insert(
            "history".to_string(),
            serde_json::to_value(
                self.inner
                    .tasks
                    .history
                    .get_task_history(&task.id)?
                    .unwrap_or_default(),
            )
            .map_err(|error| OrbitError::Execution(format!("serialize history: {error}")))?,
        );
        Ok(value)
    }

    fn add_task(
        &self,
        input: Value,
        agent: Option<String>,
        model: Option<String>,
    ) -> Result<Value, OrbitError> {
        let actor = Self::actor(agent.as_deref(), model.as_deref());
        let context_files = optional_csv_or_string_list_alias(&input, &["context_files"])?
            .unwrap_or_default()
            .into_iter()
            .map(|entry| {
                canonical_selector(&entry)
                    .map_err(|error| OrbitError::InvalidInput(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let relations = super::input::parse_relations(&input)?.unwrap_or_default();
        let task_type = optional_string_alias(&input, &["type", "task_type", "taskType"])?
            .map(|value| super::input::parse_task_type("type", &value))
            .transpose()?
            .unwrap_or(TaskType::Feature);
        let title = redact_all(&required_string(&input, &["title"], "title")?);
        let description = redact_all(&required_string(&input, &["description"], "description")?);
        let acceptance_criteria = optional_string_list_alias(
            &input,
            &[
                "acceptance_criteria",
                "acceptanceCriteria",
                "acceptance-criteria",
            ],
        )?
        .unwrap_or_default()
        .into_iter()
        .map(|criterion| redact_all(&criterion))
        .collect();
        let task = self.inner.tasks.task.create_task(TaskCreateParams {
            actor: actor.clone(),
            parent_id: None,
            title,
            description,
            acceptance_criteria,
            dependencies: Vec::new(),
            relations,
            tags: normalize_task_tags(
                optional_csv_or_string_list_alias(&input, &["tags", "tag"])?.unwrap_or_default(),
            ),
            required_tools: normalize_required_tools(
                optional_csv_or_string_list_alias(
                    &input,
                    &["required_tools", "requiredTools", "required-tool"],
                )?
                .unwrap_or_default(),
            ),
            plan: String::new(),
            execution_summary: String::new(),
            context_files,
            workspace_path: None,
            repo_root: None,
            created_by: Some(actor),
            planned_by: None,
            implemented_by: None,
            status: TaskStatus::Proposed,
            priority: optional_string(&input, "priority")?
                .map(|value| super::input::parse_task_priority("priority", &value))
                .transpose()?
                .unwrap_or(TaskPriority::Medium),
            complexity: Some(super::input::parse_assessed_task_complexity(
                "complexity",
                &required_string(&input, &["complexity"], "complexity")?,
            )?),
            task_type,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: optional_string(&input, "crew")?,
            orchestrator: optional_string(&input, "orchestrator")?,
            comments: Vec::new(),
        })?;
        self.task_json(&task)
    }

    fn update_task(
        &self,
        input: Value,
        agent: Option<String>,
        model: Option<String>,
        owner: Option<ReservationOwnerContext>,
    ) -> Result<Value, OrbitError> {
        if ["required_tools", "requiredTools", "required-tool"]
            .iter()
            .any(|field| input.get(*field).is_some())
        {
            return Err(OrbitError::InvalidInput(
                "orbit.task.update does not accept `required_tools`; task tool requirements are immutable after creation"
                    .to_string(),
            ));
        }
        let id = required_string(&input, &["id"], "id")?;
        let actor = Self::actor(agent.as_deref(), model.as_deref());

        // The pre-lock snapshot exists only as a deterministic regression seam:
        // production decisions always use the fresh snapshot read below while
        // holding the authoritative task lock.
        #[cfg(test)]
        {
            let hook = self
                .inner
                .update_hooks
                .after_pre_state_read
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(hook) = hook {
                let pre_state = self.task(&id)?;
                hook(&pre_state, &input);
            }
        }

        let mut outcome = None;
        self.inner.tasks.task.with_task_write_lock(&id, &mut || {
            let current = self.task(&id)?;
            #[cfg(test)]
            Self::update_hook(
                &self.inner.update_hooks.after_locked_state_read,
                &current,
                &input,
            );
            let updated = self.update_task_from_snapshot(
                &id,
                input.clone(),
                &actor,
                &current,
                owner.as_ref(),
            )?;
            outcome = Some((current, updated));
            Ok(())
        })?;
        let (current, updated) = outcome.ok_or_else(|| {
            OrbitError::Execution(format!("task write lock did not execute update for '{id}'"))
        })?;
        self.finish_task_update(&id, &current, &updated)
    }

    fn update_task_from_snapshot(
        &self,
        id: &str,
        input: Value,
        actor: &str,
        current: &Task,
        owner: Option<&ReservationOwnerContext>,
    ) -> Result<Task, OrbitError> {
        let status = optional_string(&input, "status")?
            .map(|value| super::input::parse_task_status("status", &value))
            .transpose()?;
        let dependencies = optional_csv_or_string_list_alias(&input, &["dependencies"])?
            .map(normalize_task_dependencies)
            .transpose()?;
        if let Some(dependencies) = dependencies.as_ref() {
            validate_task_dependencies(
                &self.inner.tasks.task.list_tasks()?,
                Some(id),
                dependencies,
            )?;
        }
        let context_files =
            optional_csv_or_string_list_alias(&input, &["context_files", "context"])?
                .map(|entries| {
                    entries
                        .into_iter()
                        .map(|entry| {
                            canonical_selector(&entry)
                                .map_err(|error| OrbitError::InvalidInput(error.to_string()))
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?;
        let artifacts = super::input::parse_artifacts(&input)?;
        let relations = super::input::parse_relations(&input)?;
        if status == Some(TaskStatus::Done)
            && current.status != TaskStatus::Done
            && let Ok(frictions) = self.friction_store()
        {
            let mut preview = current.clone();
            if let Some(relations) = &relations {
                preview.relations = relations.clone();
            }
            crate::application::task::ensure_resolves_targets_are_workspace_local(
                frictions.as_ref(),
                &self.inner.workspace_id,
                &preview,
            )?;
        }
        let comment = optional_string(&input, "comment")?;
        let task_type = optional_string_alias(&input, &["type", "task_type", "taskType"])?
            .map(|value| super::input::parse_task_type("type", &value))
            .transpose()?;
        let description = input
            .get("description")
            .map(|value| {
                value.as_str().map(redact_all).ok_or_else(|| {
                    OrbitError::InvalidInput("`description` must be a string".to_string())
                })
            })
            .transpose()?;
        let plan = input
            .get("plan")
            .map(|value| {
                value
                    .as_str()
                    .map(redact_all)
                    .ok_or_else(|| OrbitError::InvalidInput("`plan` must be a string".to_string()))
            })
            .transpose()?;
        let explicit_planned_by = raw_clearable(&input, "planned_by")?;
        let explicit_implemented_by = raw_clearable(&input, "implemented_by")?;
        let orchestrator = raw_clearable(&input, "orchestrator")?;
        if orchestrator.is_some()
            && !matches!(current.status, TaskStatus::Proposed | TaskStatus::Backlog)
        {
            return Err(OrbitError::InvalidInput(format!(
                "task {id} is {}; orchestrator can only be changed while proposed or backlog",
                current.status
            )));
        }
        self.inner.tasks.document.update_task_document(
            id,
            TaskDocumentUpdateParams {
                actor: actor.to_string(),
                title: optional_string(&input, "title")?.map(|value| redact_all(&value)),
                description,
                acceptance_criteria: optional_string_list_alias(
                    &input,
                    &[
                        "acceptance_criteria",
                        "acceptanceCriteria",
                        "acceptance-criteria",
                    ],
                )?
                .map(|values| values.into_iter().map(|value| redact_all(&value)).collect()),
                dependencies,
                relations,
                tags: optional_csv_or_string_list_alias(&input, &["tags", "tag"])?
                    .map(normalize_task_tags),
                plan,
                execution_summary: optional_raw_string(&input, "execution_summary")?
                    .map(|value| redact_all(&value)),
                context_files,
                planned_by: explicit_planned_by
                    .or_else(|| input.get("plan").is_some().then(|| Some(actor.to_string()))),
                implemented_by: explicit_implemented_by.or_else(|| {
                    (current.implemented_by.is_none()
                        && input.get("execution_summary").is_some()
                        && status.is_some_and(|value| {
                            matches!(value, TaskStatus::Review | TaskStatus::Done)
                        }))
                    .then(|| Some(actor.to_string()))
                }),
                priority: None,
                complexity: None,
                task_type,
                external_refs: None,
                pr_status: raw_clearable(&input, "pr_status")?,
                source_task_id: raw_clearable(&input, "source_task_id")?,
                job_run_id: raw_clearable(&input, "job_run_id")?,
                crew: raw_clearable(&input, "crew")?,
                orchestrator,
                created_by: None,
            },
        )?;
        if !artifacts.is_empty() {
            self.inner.tasks.artifact.upsert_task_artifacts(
                id,
                TaskArtifactUpdateParams {
                    owner_run_id: owner.map(|o| o.owner_run_id.clone()),
                    actor: actor.to_string(),
                    upsert_artifacts: artifacts,
                },
            )?;
        }
        if status.is_some() || comment.is_some() {
            let append_comments = comment
                .map(|message| TaskComment {
                    at: Utc::now(),
                    by: actor.to_string(),
                    message: redact_all(message.trim()),
                })
                .into_iter()
                .collect();
            self.inner.tasks.history.update_task_history(
                id,
                TaskHistoryUpdateParams {
                    actor: actor.to_string(),
                    status,
                    // The store emits `status_changed` only for an actual
                    // transition and includes both status values. Supplying a
                    // same-status edit therefore creates no lifecycle event.
                    status_event: None,
                    status_note: None,
                    append_history: Vec::new(),
                    append_comments,
                    // A human/agent update is authoritative by itself; it has no
                    // earlier read to guard against [ORB-11305].
                    expected_status: None,
                },
            )?;
        }
        self.task(id)
    }

    fn finish_task_update(
        &self,
        id: &str,
        current: &Task,
        updated: &Task,
    ) -> Result<Value, OrbitError> {
        if updated.status == TaskStatus::Done
            && current.status != TaskStatus::Done
            && let Ok(frictions) = self.friction_store()
        {
            for relation in &updated.relations {
                if relation.relation_type == orbit_types::task::TaskRelationType::Resolves
                    && is_valid_friction_id(&relation.target)
                    && let Err(error) =
                        frictions.auto_resolve_by_task(&relation.target, id, Utc::now())
                {
                    tracing::warn!(
                        task_id = id,
                        friction_id = relation.target,
                        error = %error,
                        "checkoutless task completion could not apply friction side effect"
                    );
                }
            }
        }
        self.task_json(updated)
    }

    fn transition(
        &self,
        input: Value,
        agent: Option<String>,
        model: Option<String>,
        start: bool,
    ) -> Result<Value, OrbitError> {
        let id = required_string(&input, &["id"], "id")?;
        let actor = Self::actor(agent.as_deref(), model.as_deref());
        let comment = optional_string(&input, "note")?.or(optional_string(&input, "comment")?);
        let crew = optional_string(&input, "crew")?;
        let mut outcome = None;
        self.inner
            .tasks
            .task
            .with_task_write_lock(&id, &mut || {
                let current = self.task(&id)?;
                let target = if start {
                    if !matches!(
                        current.status,
                        TaskStatus::Proposed
                            | TaskStatus::Backlog
                            | TaskStatus::Someday
                            | TaskStatus::Blocked
                    ) {
                        return Err(OrbitError::InvalidInput(format!(
                            "task '{id}' is in status '{}'; start requires proposed, backlog, someday, or blocked",
                            current.status
                        )));
                    }
                    if current.plan.trim().is_empty() {
                        return Err(OrbitError::InvalidInput(format!(
                            "task '{id}' requires a non-empty plan before entering in-progress"
                        )));
                    }
                    TaskStatus::InProgress
                } else {
                    match current.status {
                        TaskStatus::Proposed => TaskStatus::Backlog,
                        TaskStatus::Review => TaskStatus::Done,
                        other => {
                            return Err(OrbitError::InvalidInput(format!(
                                "task '{id}' is in status '{other}'; approve requires proposed or review"
                            )));
                        }
                    }
                };
                let mut update = Map::new();
                update.insert("id".to_string(), Value::String(id.clone()));
                update.insert("status".to_string(), Value::String(target.to_string()));
                if let Some(comment) = &comment {
                    update.insert("comment".to_string(), Value::String(comment.clone()));
                }
                if let Some(crew) = &crew {
                    update.insert("crew".to_string(), Value::String(crew.clone()));
                }
                let updated = self.update_task_from_snapshot(
                    &id,
                    Value::Object(update),
                    &actor,
                    &current,
                    None,
                )?;
                outcome = Some((current, updated));
                Ok(())
            })?;
        let (current, updated) = outcome.ok_or_else(|| {
            OrbitError::Execution(format!(
                "task write lock did not execute transition for '{id}'"
            ))
        })?;
        self.finish_task_update(&id, &current, &updated)
    }

    fn show_task(&self, input: Value) -> Result<Value, OrbitError> {
        if input
            .get("with_context")
            .or_else(|| input.get("withContext"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(OrbitError::InvalidInput(
                "checkoutless hub execution cannot provide `with_context` local-derived enrichment"
                    .to_string(),
            ));
        }
        let id = required_string(&input, &["id"], "id")?;
        let task = self.task(&id)?;
        let Some(fields) = optional_csv_or_string_list_alias(&input, &["fields", "field"])? else {
            return self.task_json(&task);
        };
        let status = self.inner.tasks.task.task_status_index()?;
        let one = |field: &str| -> Result<Value, OrbitError> {
            if let Some(value) = task_show_record_field_json(&task, field) {
                return Ok(value);
            }
            match field {
                "comments" => serde_json::to_value(
                    self.inner
                        .tasks
                        .history
                        .get_task_comments(&id)?
                        .unwrap_or_default(),
                )
                .map_err(|error| OrbitError::Execution(error.to_string())),
                "history" => serde_json::to_value(
                    self.inner
                        .tasks
                        .history
                        .get_task_history(&id)?
                        .unwrap_or_default(),
                )
                .map_err(|error| OrbitError::Execution(error.to_string())),
                "artifacts" => Ok(super::json::serialize_task_artifacts(
                    &self
                        .inner
                        .tasks
                        .artifact
                        .get_task_artifacts(&id)?
                        .unwrap_or_default(),
                )),
                "plan" => Ok(json!(task.plan)),
                "execution_summary" => Ok(json!(task.execution_summary)),
                "description" => Ok(json!(task.description)),
                "acceptance_criteria" => Ok(json!(task.acceptance_criteria)),
                "dependencies" => Ok(json!(task.dependencies())),
                "resolved_dependencies" => Ok(json!(
                    resolve_task_dependencies(&task, &status)
                        .into_iter()
                        .map(|entry| entry.label())
                        .collect::<Vec<_>>()
                )),
                "relations" => Ok(json!(resolve_task_relations(&task, &status))),
                "tags" => Ok(json!(task.tags)),
                "context_files" => Ok(json!(task.context_files)),
                "crew" => Ok(json!(task.crew)),
                "orchestrator" => Ok(json!(task.orchestrator)),
                other => Err(OrbitError::InvalidInput(unknown_task_show_field_message(
                    other,
                ))),
            }
        };
        if fields.len() == 1 {
            return one(&fields[0]);
        }
        let mut object = Map::new();
        for field in fields {
            object.insert(field.clone(), one(&field)?);
        }
        Ok(Value::Object(object))
    }

    fn list_tasks(&self, input: Value) -> Result<Value, OrbitError> {
        let status_filter = optional_csv_or_string_list_alias(&input, &["status"])?
            .map(|values| {
                values
                    .into_iter()
                    .map(|value| super::input::parse_task_status("status", &value))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        let type_filter = optional_string_alias(&input, &["type", "task_type", "taskType"])?
            .map(|value| super::input::parse_task_type("type", &value))
            .transpose()?;
        let tags = optional_csv_or_string_list_alias(&input, &["tags", "tag"])?.unwrap_or_default();
        let ready = super::input::optional_bool_alias(&input, &["ready"])?;
        let limit = super::input::task_list_limit(&input)?;
        let page = crate::application::task::query_task_store(
            self.inner.tasks.task.as_ref(),
            &crate::application::task::TaskListQuery {
                filter: crate::application::task::TaskListFilter {
                    statuses: status_filter,
                    task_type: type_filter,
                    tags,
                    ..Default::default()
                },
                ready: ready == Some(true),
                limit,
                path: None,
            },
        )?;
        let status = page.status_by_id;
        Ok(Value::Array(
            page.items
                .into_iter()
                .map(|row| super::json::task_to_json(&row.task, &status))
                .collect(),
        ))
    }

    fn friction_root(&self) -> Result<PathBuf, OrbitError> {
        prepare_hub_friction_root(
            &self.inner.global_root,
            &self.inner.workspace_id,
            self.inner.legacy_friction_root.as_deref(),
        )
    }

    fn readable_friction_root(&self) -> Result<PathBuf, OrbitError> {
        readable_hub_friction_root(
            &self.inner.global_root,
            &self.inner.workspace_id,
            self.inner.legacy_friction_root.as_deref(),
        )
    }

    /// The hub's friction store, partitioned by the logical workspace ID.
    ///
    /// Records live in the host-global SQLite database; the hub file tree is
    /// still resolved because it carries the tag taxonomy and, until the
    /// one-time import commits, the legacy records to import. `friction_root`
    /// publishes the checkout-local tree into the canonical hub location
    /// first, so a workspace that never opened the hub before still imports
    /// its own history exactly once.
    fn friction_store(
        &self,
    ) -> Result<Arc<dyn orbit_store::contracts::FrictionStoreBackend>, OrbitError> {
        let files_root = match self.friction_root() {
            Ok(root) => root,
            Err(_) => self.readable_friction_root()?,
        };
        let database = orbit_config::resolved_audit_db_path(
            &orbit_config::ConfigRoots::global_only(&self.inner.global_root),
        )?;
        workspace_friction_store(
            Store::open(&database)?,
            self.inner.workspace_id.clone(),
            files_root,
        )
    }

    /// The hub-coordination half of the friction handler table.
    ///
    /// Exhaustive over [`FrictionVerb`] on purpose (ADR-0209 bearing 1,
    /// ORB-10358): a new friction verb has to state here whether the
    /// checkoutless hub can serve it, rather than silently falling through.
    fn friction(
        &self,
        verb: FrictionVerb,
        input: Value,
        model: Option<String>,
    ) -> Result<Value, OrbitError> {
        match verb {
            FrictionVerb::List => {
                let mut value =
                    super::friction_tools::list_in(self.friction_store()?.as_ref(), input)?;
                strip_private_friction_paths(&mut value);
                Ok(value)
            }
            FrictionVerb::Show => {
                let mut value =
                    super::friction_tools::show_in(self.friction_store()?.as_ref(), input)?;
                strip_private_friction_paths(&mut value);
                Ok(value)
            }
            FrictionVerb::Tags => Ok(json!(self.friction_store()?.tags()?)),
            FrictionVerb::Add => {
                let model = model
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        OrbitError::InvalidInput("orbit.friction.add requires `model`".to_string())
                    })?;
                let stored = self.friction_store()?.add(FrictionAddParams {
                    model,
                    title: optional_raw_string(&input, "title")?
                        .map(|raw| normalize_title(&redact_all(&raw)))
                        .transpose()?,
                    body: redact_all(&required_string(&input, &["body", "description"], "body")?),
                    tags: optional_csv_or_string_list_alias(&input, &["tags", "tag"])?
                        .unwrap_or_default(),
                    during_task: optional_string(&input, "during_task")?
                        .or(optional_string(&input, "task_id")?),
                    created_at: Utc::now(),
                })?;
                friction_json(stored)
            }
            FrictionVerb::Update => {
                let id = required_string(&input, &["id"], "id")?;
                let status = optional_string(&input, "status")?
                    .map(|value| {
                        FrictionStatus::from_str(&value)
                            .map_err(|error| OrbitError::InvalidInput(format!("`status` {error}")))
                    })
                    .transpose()?;
                let tags = optional_csv_or_string_list_alias(&input, &["tags", "tag"])?;
                let body = optional_string(&input, "body")?.map(|value| redact_all(&value));
                let title = match optional_raw_string(&input, "title")? {
                    None => None,
                    Some(raw) if raw.trim().is_empty() => Some(None),
                    Some(raw) => Some(Some(normalize_title(&redact_all(&raw))?)),
                };
                if status.is_none() && tags.is_none() && body.is_none() && title.is_none() {
                    return Err(OrbitError::InvalidInput(
                        "orbit.friction.update requires `status`, `tags`, `body`, or `title`"
                            .to_string(),
                    ));
                }
                friction_json(self.friction_store()?.update(
                    &id,
                    FrictionUpdateParams {
                        status,
                        tags,
                        title,
                        body,
                        resolved_by_task: None,
                        updated_at: Utc::now(),
                    },
                )?)
            }
            // Aggregate stats need the task store, and resolution is an
            // operator action taken against a real checkout.
            FrictionVerb::Stats | FrictionVerb::Resolve => Err(OrbitError::InvalidInput(format!(
                "action {} is outside the checkoutless hub coordination executor",
                verb.tool_name()
            ))),
        }
    }
}

impl OrbitToolHost for HubCoordinationExecutor {
    fn execute(
        &self,
        action: OrbitBuiltinAction,
        input: Value,
        agent: Option<String>,
        model: Option<String>,
        reservation_owner: Option<ReservationOwnerContext>,
    ) -> Result<Value, OrbitError> {
        let (input, _redaction_report) =
            super::artifact_redaction::sanitize_tool_input(action, input)?;
        match action {
            OrbitBuiltinAction::TaskAdd => self.add_task(input, agent, model),
            OrbitBuiltinAction::TaskApprove => self.transition(input, agent, model, false),
            OrbitBuiltinAction::TaskStart => self.transition(input, agent, model, true),
            OrbitBuiltinAction::TaskShow => self.show_task(input),
            OrbitBuiltinAction::TaskList => self.list_tasks(input),
            OrbitBuiltinAction::TaskUpdate => {
                self.update_task(input, agent, model, reservation_owner)
            }
            OrbitBuiltinAction::Friction(verb) => self.friction(verb, input, model),
            _ => Err(OrbitError::InvalidInput(format!(
                "action {action:?} is outside the checkoutless hub coordination executor"
            ))),
        }
    }

    fn task_scope(&self) -> OrbitTaskScope {
        OrbitTaskScope::default()
    }
}

fn raw_clearable(input: &Value, field: &str) -> Result<Option<Option<String>>, OrbitError> {
    Ok(optional_raw_string(input, field)?.map(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    }))
}

fn friction_json(
    stored: orbit_store::friction_store::StoredFrictionRecord,
) -> Result<Value, OrbitError> {
    let record = &stored.record;
    let mut value = serde_json::to_value(record)
        .map_err(|error| OrbitError::Store(format!("serialize friction record: {error}")))?;
    // Match the checkout-backed projection: `title` is always on the wire, so
    // a hub consumer never has to know derivation exists.
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "title".to_string(),
            json!(effective_title(
                record.title.as_deref(),
                &record.body,
                &record.id
            )),
        );
    }
    Ok(value)
}

fn strip_private_friction_paths(value: &mut Value) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(strip_private_friction_paths),
        Value::Object(object) => {
            object.remove("path");
            if let Some(records) = object.get_mut("records") {
                strip_private_friction_paths(records);
            }
        }
        _ => {}
    }
}

impl OrbitToolHost for RuntimeOrbitToolHost {
    fn execute(
        &self,
        action: OrbitBuiltinAction,
        input: Value,
        agent: Option<String>,
        model: Option<String>,
        reservation_owner: Option<ReservationOwnerContext>,
    ) -> Result<Value, OrbitError> {
        let (agent, model) = self
            .runtime
            .try_canonical_agent_model_identity(agent.as_deref(), model.as_deref())?;
        super::dispatch::execute(
            &self.runtime,
            &self.task_scope,
            super::dispatch::ToolCaller {
                session_context: &self.session_context,
                agent,
                model,
                reservation_owner,
            },
            action,
            input,
        )
    }

    fn task_scope(&self) -> OrbitTaskScope {
        self.task_scope.clone()
    }
}

fn trusted_env_run_id() -> Option<String> {
    managed_run_context_run_id_from_env()
}

#[cfg(test)]
mod checkoutless_hub_tests {
    use super::*;

    fn executor() -> (
        tempfile::TempDir,
        HubCoordinationExecutor,
        ToolSessionContext,
    ) {
        let root = tempfile::tempdir().expect("global root");
        HubCoordinationExecutor::register_workspace(root.path(), "ws_checkoutless", "checkoutless")
            .expect("register workspace");
        let executor = HubCoordinationExecutor::new(root.path(), "ws_checkoutless", None)
            .expect("coordination executor");
        let context = ToolSessionContext::trusted_local(
            Some("ws_checkoutless".to_string()),
            Some("hm_hub".to_string()),
            Some("hub".to_string()),
        );
        (root, executor, context)
    }

    #[test]
    fn task_artifact_review_and_verdict_persist_without_checkout() {
        let (root, executor, context) = executor();
        let created = executor
            .execute_tool(
                "orbit.task.add",
                json!({
                    "workspace": "ws_checkoutless",
                    "title": "Coordinate from hub",
                    "description": "No checkout required",
                    "complexity": "low",
                    "context_files": ["future/new.rs", "symbol:src/lib.rs#Host:struct"],
                    "model": "codex"
                }),
                context.clone(),
            )
            .expect("add checkoutless task");
        let id = created["id"].as_str().expect("task id");
        assert_eq!(created["context_files"][0], "file:future/new.rs");
        assert!(created.get("resolved_crew").is_none());

        executor
            .execute_tool(
                "orbit.task.update",
                json!({
                    "id": id,
                    "plan": "1. Execute",
                    "status": "in_progress",
                    "execution_summary": "Outcome: success",
                    "model": "codex"
                }),
                context.clone(),
            )
            .expect("persist verdict fields");

        let mut ssh_context = context.clone();
        ssh_context.transport = Some(orbit_types::tool::McpTransport::SshMcp);
        let with_artifact = executor
            .execute_tool(
                "orbit.task.artifact.put",
                json!({
                    "id": id,
                    "artifacts": [{"path": "reports/result.txt", "content": "payload"}],
                    "model": "codex"
                }),
                ssh_context,
            )
            .expect("put artifact bytes");
        assert_eq!(with_artifact["execution_summary"], "Outcome: success");

        let artifact = executor
            .execute_tool(
                "orbit.task.show",
                json!({"id": id, "fields": "artifacts"}),
                context,
            )
            .expect("show artifact");
        assert_eq!(artifact[0]["path"], "reports/result.txt");
        assert_eq!(artifact[0]["content"], "payload");
        assert!(
            !root.path().join(".orbit").exists(),
            "no fabricated checkout"
        );
    }

    #[test]
    fn task_required_tools_update_is_rejected_without_checkout() {
        let (_root, executor, context) = executor();
        let created = executor
            .execute_tool(
                "orbit.task.add",
                json!({
                    "workspace": "ws_checkoutless",
                    "title": "Immutable checkoutless authority",
                    "description": "Required tools are fixed at issuance",
                    "complexity": "low",
                    "required_tools": ["proc.spawn"],
                    "model": "codex"
                }),
                context,
            )
            .expect("add checkoutless task");
        let id = created["id"].as_str().expect("task id");

        let error = OrbitToolHost::execute(
            &executor,
            OrbitBuiltinAction::TaskUpdate,
            json!({"id": id, "required_tools": ["orbit.task.show"]}),
            Some("codex".to_string()),
            Some("codex".to_string()),
            None,
        )
        .expect_err("checkoutless update cannot replace required tools");
        assert!(error.to_string().contains("immutable"), "{error}");
        assert_eq!(
            executor.task(id).expect("read task").required_tools,
            ["proc.spawn"]
        );
    }

    #[test]
    fn task_orchestrator_add_update_and_clear_round_trip_without_checkout() {
        let (_root, executor, context) = executor();
        let created = executor
            .execute_tool(
                "orbit.task.add",
                json!({
                    "workspace": "ws_checkoutless",
                    "title": "Canonical hub orchestrator",
                    "description": "Validate against the checkoutless registry",
                    "complexity": "low",
                    "orchestrator": "  sol  ",
                    "model": "codex"
                }),
                context.clone(),
            )
            .expect("add trims and persists orchestrator");
        let id = created["id"].as_str().expect("task id");
        assert_eq!(created["orchestrator"], "sol");

        let updated = executor
            .execute_tool(
                "orbit.task.update",
                json!({"id": id, "orchestrator": "  terra  ", "model": "codex"}),
                context.clone(),
            )
            .expect("update trims and persists orchestrator");
        assert_eq!(updated["orchestrator"], "terra");
        assert_eq!(
            executor
                .execute_tool(
                    "orbit.task.show",
                    json!({"id": id, "fields": ["crew", "orchestrator"]}),
                    context.clone(),
                )
                .expect("show execution and orchestration fields"),
            json!({"crew": null, "orchestrator": "terra"})
        );

        let cleared = executor
            .execute_tool(
                "orbit.task.update",
                json!({"id": id, "orchestrator": "", "model": "codex"}),
                context,
            )
            .expect("empty string clears orchestrator");
        assert_eq!(cleared["orchestrator"], Value::Null);
    }

    #[test]
    fn task_show_projects_public_dto_fields_without_checkout() {
        let (_root, executor, context) = executor();
        let created = executor
            .execute_tool(
                "orbit.task.add",
                json!({
                    "workspace": "ws_checkoutless",
                    "title": "Hub status projection",
                    "description": "Exercise fields:[status] on the hub.",
                    "complexity": "low",
                    "relations": [{"type": "related_to", "target": "DK-00042"}],
                    "model": "codex"
                }),
                context.clone(),
            )
            .expect("add checkoutless task");
        let id = created["id"].as_str().expect("task id");

        assert_eq!(
            executor
                .execute_tool(
                    "orbit.task.show",
                    json!({"id": id, "fields": ["status"]}),
                    context.clone(),
                )
                .expect("fields:[status] must succeed"),
            json!("proposed")
        );
        executor
            .execute_tool(
                "orbit.task.update",
                json!({"id": id, "job_run_id": "jrun-hub", "model": "codex"}),
                context.clone(),
            )
            .expect("attach checkoutless job run");
        assert_eq!(
            executor
                .execute_tool(
                    "orbit.task.show",
                    json!({
                        "id": id,
                        "fields": ["status", "relations", "external_refs", "job_run_id"],
                    }),
                    context,
                )
                .expect("mixed projection must succeed"),
            json!({
                "status": "proposed",
                "relations": [{
                    "type": "related_to",
                    "target": "DK-00042",
                    "verification": "not verifiable here",
                }],
                "external_refs": [],
                "job_run_id": "jrun-hub",
            })
        );
    }

    /// After ORB-10680 the partition is the `(workspace_id, friction_id)` key
    /// in the host-global store, not a per-workspace directory: a record filed
    /// through one workspace's hub executor is invisible to another's.
    #[test]
    fn friction_writes_use_the_workspace_sqlite_partition() {
        let (root, executor, context) = executor();
        let result = executor
            .execute_tool(
                "orbit.friction.add",
                json!({"body": "Hub friction", "tags": ["tooling"], "model": "codex"}),
                context.clone(),
            )
            .expect("add friction");
        assert!(result.get("path").is_none(), "hub responses are path-free");
        let id = result["id"].as_str().expect("friction id").to_string();

        let listed = executor
            .execute_tool("orbit.friction.list", json!({}), context.clone())
            .expect("list frictions");
        assert_eq!(listed.as_array().map(Vec::len), Some(1));
        assert_eq!(listed[0]["id"], json!(id));

        HubCoordinationExecutor::register_workspace(root.path(), "ws_other", "other")
            .expect("register second workspace");
        let other = HubCoordinationExecutor::new(root.path(), "ws_other", None)
            .expect("second coordination executor");
        let other_listed = other
            .execute_tool("orbit.friction.list", json!({}), context)
            .expect("list frictions in the other workspace");
        assert_eq!(
            other_listed.as_array().map(Vec::len),
            Some(0),
            "a second workspace must not see the first workspace's records"
        );
    }

    #[test]
    fn task_list_accepts_multi_status_filters_without_checkout() {
        let (_root, executor, context) = executor();
        for (title, status) in [
            ("Backlog hub task", "backlog"),
            ("In-progress hub task", "in-progress"),
            ("Review hub task", "review"),
            ("Done hub task", "done"),
        ] {
            let created = executor
                .execute_tool(
                    "orbit.task.add",
                    json!({
                        "workspace": "ws_checkoutless",
                        "title": title,
                        "description": "multi-status fixture",
                        "complexity": "low",
                        "model": "codex"
                    }),
                    context.clone(),
                )
                .expect("add checkoutless task");
            executor
                .execute_tool(
                    "orbit.task.update",
                    json!({
                        "id": created["id"],
                        "status": status,
                        "plan": "Multi-status filter fixture.",
                        "model": "codex"
                    }),
                    context.clone(),
                )
                .expect("set checkoutless task status");
        }

        for input in [
            json!({ "status": "backlog,in-progress,review" }),
            json!({ "status": ["backlog", "in-progress", "review"] }),
        ] {
            let output = executor
                .execute_tool("orbit.task.list", input, context.clone())
                .expect("multi-status filter succeeds");
            let statuses = output
                .as_array()
                .expect("task array")
                .iter()
                .map(|task| task["status"].as_str().expect("task status"))
                .collect::<std::collections::HashSet<_>>();
            assert_eq!(statuses, ["backlog", "in-progress", "review"].into());
        }
    }
}
