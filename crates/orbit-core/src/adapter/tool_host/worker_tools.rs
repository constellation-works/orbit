//! Generic coordination writes translated into owner claim transactions.
use orbit_common::OrbitError;
use orbit_common::governance::friction::FrictionVerb;
use orbit_store::contracts::{
    ClaimEvidence, ClaimInvocation, ClaimMutation, ClaimRun, ClaimWorkerUpdate,
};
use orbit_tools::OrbitBuiltinAction;
use orbit_types::tool::ToolSessionContext;
use serde_json::Value;

use crate::OrbitRuntime;

pub(crate) fn execute(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    action: OrbitBuiltinAction,
    input: &Value,
    model: Option<&str>,
) -> Result<Option<Value>, OrbitError> {
    if !matches!(
        action,
        OrbitBuiltinAction::TaskUpdate
            | OrbitBuiltinAction::TaskShow
            | OrbitBuiltinAction::TaskList
            | OrbitBuiltinAction::TaskArtifactGet
            | OrbitBuiltinAction::TaskLint
            | OrbitBuiltinAction::TaskLocks
            | OrbitBuiltinAction::TaskAdd
            | OrbitBuiltinAction::TaskDelete
            | OrbitBuiltinAction::TaskReject
            | OrbitBuiltinAction::TaskLocksRelease
            | OrbitBuiltinAction::TaskLocksReserve
            | OrbitBuiltinAction::AutoTaskAdd
            | OrbitBuiltinAction::AutoTaskUpdate
            | OrbitBuiltinAction::AutoTaskToggle
            | OrbitBuiltinAction::Friction(_)
    ) {
        return Ok(None);
    }
    let Some(binding) = &session.worker_invocation else {
        return Ok(None);
    };
    binding.validate().map_err(OrbitError::InvalidInput)?;
    let machine = session
        .process_machine_id
        .as_deref()
        .or_else(|| runtime.automation_machine_identity());
    if machine != Some(binding.owner_machine_id.as_str())
        || runtime.workspace_id()? != binding.owner_workspace_id
    {
        return Err(OrbitError::PolicyDenied(
            "worker owner destination mismatch".into(),
        ));
    }
    if action == OrbitBuiltinAction::TaskShow
        && let Some(projection) = input.get("_worker_read").and_then(Value::as_str)
    {
        let id = input.get("id").and_then(Value::as_str).unwrap_or_default();
        let value = match projection {
            "tags" => serde_json::to_value(runtime.list_tasks_by_tags(
                &optional_field::<Vec<String>>(input, "tags")?.unwrap_or_default(),
            )?),
            "search" => serde_json::to_value(
                runtime.search_tasks_filtered(
                    input
                        .get("query")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    &optional_field::<Vec<String>>(input, "tags")?.unwrap_or_default(),
                )?,
            ),
            "filtered" => serde_json::to_value(runtime.list_tasks_filtered(
                optional_field(input, "status")?,
                optional_field(input, "priority")?,
                input.get("parent_id").and_then(Value::as_str),
                input.get("job_run_id").and_then(Value::as_str),
                optional_field(input, "external_ref")?.as_ref(),
                input.get("has_external_ref_system").and_then(Value::as_str),
            )?),
            "tasks" => serde_json::to_value(runtime.list_tasks()?),
            "status_index" => serde_json::to_value(runtime.task_status_index()?),
            "completion_by_complexity" => serde_json::to_value(
                runtime
                    .task_completion_by_complexity()?
                    .into_iter()
                    .map(|row| (row.complexity, row.total, row.by_status))
                    .collect::<Vec<_>>(),
            ),
            "complexity_by_id" => serde_json::to_value(runtime.task_complexity_by_id()?),
            "task" => serde_json::to_value(runtime.get_task(id)?),
            "artifacts" => serde_json::to_value(runtime.get_task_artifacts(id)?),
            "manifest" => serde_json::to_value(runtime.get_task_artifact_manifest(id)?),
            "comments" => serde_json::to_value(runtime.get_task_comments(id)?),
            "history" => serde_json::to_value(runtime.get_task_history(id)?),
            "dependency" => match runtime.resolve_dependency_task(id)? {
                orbit_store::RegisteredTaskResolution::Resolved(task) => serde_json::to_value(task),
                _ => {
                    return Err(OrbitError::InvalidInput(
                        "owner dependency unavailable".into(),
                    ));
                }
            },
            _ => {
                return Err(OrbitError::InvalidInput(
                    "unknown worker read projection".into(),
                ));
            }
        }
        .map_err(|error| OrbitError::Store(error.to_string()))?;
        return Ok(Some(value));
    }
    let mut friction_tag_substitutions = Vec::new();
    let mutation = match action {
        OrbitBuiltinAction::TaskUpdate => {
            binding
                .validate_arguments(input)
                .map_err(OrbitError::InvalidInput)?;
            if input
                .get("id")
                .is_some_and(|id| id.as_str() != Some(&binding.task_id))
            {
                return Err(OrbitError::PolicyDenied(
                    "worker task binding mismatch".into(),
                ));
            }
            if let Some(update) = input.get("_worker_update") {
                let update: ClaimWorkerUpdate = serde_json::from_value(update.clone())
                    .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
                if !update.evidence.artifacts.is_empty() {
                    return Err(OrbitError::InvalidInput(
                        "use the task artifact adapter for artifact payloads".into(),
                    ));
                }
                return apply(runtime, session, ClaimMutation::Update(update), Vec::new())
                    .map(Some);
            }
            orbit_common::protocol::tool_input::reject_unknown_tool_fields(
                input,
                &[
                    "id",
                    "task_id",
                    "claim_id",
                    "bound_run_id",
                    "execution_summary",
                    "plan",
                    "context_files",
                    "allow_missing_context",
                    "status",
                    "external_refs",
                    "comment",
                    "artifacts",
                    "model",
                    "agent",
                    "workspace",
                    "field",
                    "fields",
                ],
            )?;
            ClaimMutation::Update(ClaimWorkerUpdate {
                plan: orbit_common::protocol::tool_input::optional_raw_string(input, "plan")?,
                context_files:
                    orbit_common::protocol::tool_input::optional_csv_or_string_list_alias(
                        input,
                        &["context_files"],
                    )?,
                status: optional_field(input, "status")?,
                external_refs: optional_field(input, "external_refs")?.unwrap_or_default(),
                evidence: ClaimEvidence {
                    summary: orbit_common::protocol::tool_input::optional_raw_string(
                        input,
                        "execution_summary",
                    )?,
                    comment: orbit_common::protocol::tool_input::optional_raw_string(
                        input, "comment",
                    )?,
                    artifacts: super::input::parse_artifacts(input)?,
                },
                ..Default::default()
            })
        }
        OrbitBuiltinAction::Friction(FrictionVerb::Add) => {
            binding
                .validate_arguments(input)
                .map_err(OrbitError::InvalidInput)?;
            let (mut params, substitutions) =
                super::friction_tools::add_params(input, model.map(str::to_owned))?;
            friction_tag_substitutions = substitutions;
            params.during_task = Some(binding.task_id.clone());
            let taxonomy = crate::runtime::friction::store_for(runtime)?
                .tags()?
                .into_iter()
                .collect();
            params.tags = orbit_store::contracts::normalize_friction_tags(params.tags, &taxonomy)?;
            ClaimMutation::Friction(params)
        }
        OrbitBuiltinAction::TaskShow
        | OrbitBuiltinAction::TaskList
        | OrbitBuiltinAction::TaskArtifactGet
        | OrbitBuiltinAction::TaskLint
        | OrbitBuiltinAction::TaskLocks
        | OrbitBuiltinAction::Friction(
            FrictionVerb::List | FrictionVerb::Show | FrictionVerb::Stats | FrictionVerb::Tags,
        ) => return Ok(None),
        // These writes use the owner's checkout-backed definition root. They
        // must pass the worker destination check above before ordinary CRUD.
        OrbitBuiltinAction::AutoTaskAdd
        | OrbitBuiltinAction::AutoTaskUpdate
        | OrbitBuiltinAction::AutoTaskToggle => {
            binding
                .validate_arguments(input)
                .map_err(OrbitError::InvalidInput)?;
            return Ok(None);
        }
        OrbitBuiltinAction::TaskAdd
        | OrbitBuiltinAction::TaskDelete
        | OrbitBuiltinAction::TaskReject
        | OrbitBuiltinAction::TaskLocksRelease
        | OrbitBuiltinAction::TaskLocksReserve
        | OrbitBuiltinAction::Friction(_) => {
            return Err(OrbitError::PolicyDenied(
                "claimed worker operation requires its lifecycle boundary".into(),
            ));
        }
        _ => return Ok(None),
    };
    apply(runtime, session, mutation, friction_tag_substitutions).map(Some)
}

fn optional_field<T: serde::de::DeserializeOwned>(
    input: &Value,
    name: &str,
) -> Result<Option<T>, OrbitError> {
    input
        .get(name)
        .filter(|value| !value.is_null())
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|error| OrbitError::InvalidInput(error.to_string()))
        })
        .transpose()
}

fn apply(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    mutation: ClaimMutation,
    friction_tag_substitutions: Vec<(String, String)>,
) -> Result<Value, OrbitError> {
    let binding = session
        .worker_invocation
        .as_ref()
        .ok_or_else(|| OrbitError::PolicyDenied("worker binding missing".into()))?;
    let auth = ClaimInvocation::trusted_worker(
        binding.task_id.clone(),
        binding.claim_id.clone(),
        binding.execution.machine_id.clone(),
        Some(ClaimRun {
            machine_id: binding.execution.machine_id.clone(),
            run_id: binding.bound_run_id.clone(),
        }),
    );
    let mut identity = mutation.clone();
    if let ClaimMutation::Friction(params) = &mut identity {
        params.created_at = chrono::DateTime::UNIX_EPOCH;
    }
    let bytes = serde_json::to_vec(&identity)
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
    let mutation_id = blake3::hash(&bytes).to_hex().to_string();
    let result = runtime.mutate_execution_claim(Some(&auth), &mutation_id, &mutation)?;
    if let Some(record) = result.friction {
        return super::friction_tools::record_to_json_with_tag_normalizations(
            orbit_store::contracts::StoredFrictionRecord { record, path: None },
            friction_tag_substitutions,
        );
    }
    let task = runtime.get_task(&binding.task_id)?;
    super::json::serialize_task(runtime, &task)
}
