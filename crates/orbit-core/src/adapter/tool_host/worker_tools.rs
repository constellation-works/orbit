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
            | OrbitBuiltinAction::TaskEligible
            | OrbitBuiltinAction::TaskArtifactGet
            | OrbitBuiltinAction::TaskLint
            | OrbitBuiltinAction::TaskLocks
            | OrbitBuiltinAction::TaskAdd
            | OrbitBuiltinAction::TaskDelete
            | OrbitBuiltinAction::TaskReject
            | OrbitBuiltinAction::TaskReconcileReview
            | OrbitBuiltinAction::TaskReviewReset
            | OrbitBuiltinAction::TaskLocksRelease
            | OrbitBuiltinAction::TaskLocksReserve
            | OrbitBuiltinAction::AutoTaskAdd
            | OrbitBuiltinAction::AutoTaskUpdate
            | OrbitBuiltinAction::Friction(_)
    ) {
        return Ok(None);
    }
    crate::runtime::check_worker_host_input(input, session.worker_host_call)?;
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
            "filtered" => {
                let job_run_id = input.get("job_run_id").and_then(Value::as_str);
                let mut tasks = runtime.list_tasks_filtered(
                    optional_field(input, "status")?,
                    optional_field(input, "priority")?,
                    input.get("parent_id").and_then(Value::as_str),
                    job_run_id,
                    optional_field(input, "external_ref")?.as_ref(),
                    input.get("has_external_ref_system").and_then(Value::as_str),
                )?;
                // A leaf's run id is unique only on its own machine; the owner's
                // drain can bind the same id [ORB-13649]. Only bindings this
                // leaf's machine made belong to its run.
                if job_run_id.is_some() {
                    tasks.retain(|task| {
                        task.job_run_machine
                            .as_ref()
                            .is_some_and(|bound| bound.machine_id == binding.execution.machine_id)
                    });
                }
                serde_json::to_value(tasks)
            }
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
            "dependency_history" => serde_json::to_value(runtime.dependency_history(id)?),
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
    if action == OrbitBuiltinAction::TaskArtifactGet {
        require_active_claim(runtime, session)?;
        return Ok(None);
    }
    let mut friction_tag_substitutions = Vec::new();
    let mutation = match action {
        OrbitBuiltinAction::TaskUpdate => {
            binding
                .validate_arguments(input)
                .map_err(OrbitError::InvalidInput)?;
            // Artifact attachments can reach the mutation receipt path, which
            // intentionally reconciles a lost reply before rechecking claim
            // authority. Check the current claim first so a stale replay cannot
            // turn an old successful report upload into a new apparent success.
            if input.get("artifacts").is_some() {
                require_active_claim(runtime, session)?;
            }
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
                    ..Default::default()
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
        | OrbitBuiltinAction::TaskEligible
        | OrbitBuiltinAction::TaskLint
        | OrbitBuiltinAction::TaskLocks
        | OrbitBuiltinAction::Friction(
            FrictionVerb::List | FrictionVerb::Show | FrictionVerb::Stats | FrictionVerb::Tags,
        ) => return Ok(None),
        // These writes use the owner's checkout-backed definition root. They
        // must pass the worker destination check above before ordinary CRUD.
        OrbitBuiltinAction::AutoTaskAdd | OrbitBuiltinAction::AutoTaskUpdate => {
            binding
                .validate_arguments(input)
                .map_err(OrbitError::InvalidInput)?;
            return Ok(None);
        }
        // [ORB-14260] A claimed worker files follow-up work as ordinary
        // creation, but only work spawned from its own claimed task, and only
        // while the owner still holds that claim as active. [ORB-14792] A
        // claimed review task's findings may also name the task in this
        // workspace that introduced each, as `regression_from`.
        OrbitBuiltinAction::TaskAdd => {
            binding
                .validate_arguments(input)
                .map_err(OrbitError::InvalidInput)?;
            let findings = orbit_types::workflow::files_regression_findings(
                &runtime.get_task(&binding.task_id)?.tags,
            );
            let culprits = binding
                .validate_spawned_relations(input, findings)
                .map_err(OrbitError::PolicyDenied)?;
            for culprit in culprits {
                match runtime.get_task(&culprit) {
                    Ok(_) => {}
                    Err(OrbitError::NotFound { .. }) => {
                        return Err(OrbitError::PolicyDenied(format!(
                            "a claimed review worker's regression_from target `{culprit}` is \
                             not a task in the claimed task's workspace"
                        )));
                    }
                    Err(error) => return Err(error),
                }
            }
            require_active_claim(runtime, session)?;
            return Ok(None);
        }
        OrbitBuiltinAction::TaskDelete
        | OrbitBuiltinAction::TaskReject
        | OrbitBuiltinAction::TaskReconcileReview
        | OrbitBuiltinAction::TaskReviewReset
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

/// The claim invocation a bound worker's call acts under: its own claim and
/// bound run, never anything the call's input names.
fn worker_auth(binding: &orbit_types::tool::WorkerInvocation) -> ClaimInvocation {
    ClaimInvocation::trusted_worker(
        binding.task_id.clone(),
        binding.claim_id.clone(),
        binding.execution.machine_id.clone(),
        Some(ClaimRun {
            machine_id: binding.execution.machine_id.clone(),
            run_id: binding.bound_run_id.clone(),
        }),
    )
}

/// [ORB-14221] A worker's artifact read stands on the authority its artifact
/// write does: the owner answers only while the claim could still take that
/// worker's update. Once the claim is released, failed, revoked, landed or
/// superseded, or bound to another run, the read is refused as `stale_claim`
/// whatever the worker's own records still show — a claimed reviewer's
/// manifest read through its run's broker included.
fn require_active_claim(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
) -> Result<(), OrbitError> {
    let binding = session
        .worker_invocation
        .as_ref()
        .ok_or_else(|| OrbitError::PolicyDenied("worker binding missing".into()))?;
    runtime
        .verify_worker_claim(&worker_auth(binding))
        .map_err(|error| match error {
            OrbitError::InvalidInput(cause) if cause == "stale_claim" => OrbitError::PolicyDenied(
                "stale_claim: the owner no longer holds this worker's claim as active (it was \
                 released, failed, revoked, landed or superseded, or is bound to another run), \
                 so the owner refuses its artifact reads as it refuses its writes. Do not retry \
                 or route around the owner; report the work incomplete and let the run end"
                    .into(),
            ),
            other => other,
        })
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
    let auth = worker_auth(binding);
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
