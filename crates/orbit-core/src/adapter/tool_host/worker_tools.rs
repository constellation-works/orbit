//! Generic coordination writes translated into owner claim transactions.
use orbit_common::governance::agents::AgentsVerb;
use orbit_common::governance::auto_task::AutoTaskVerb;
use orbit_common::governance::automation::AutomationVerb;
use orbit_common::governance::codebase::CodebaseVerb;
use orbit_common::governance::drain::DrainVerb;
use orbit_common::governance::friction::FrictionVerb;
use orbit_common::governance::operation::OperationVerb;
use orbit_common::governance::search::SearchVerb;
use orbit_common::governance::task::{TASK_CLI_OPTIONS_PARAM, TaskVerb};
use orbit_common::governance::workflow::WorkflowVerb;
use orbit_common::governance::workspace_claim::WorkspaceClaimVerb;
use orbit_common::{ClaimRefusalKind, OrbitError};
use orbit_store::contracts::{
    ClaimEvidence, ClaimInvocation, ClaimMutation, ClaimRun, ClaimSteeringRecord, ClaimWorkerUpdate,
};
use orbit_tools::OrbitBuiltinAction;
use orbit_types::tool::{ToolSessionContext, WorkerInvocation};
use serde_json::Value;

use crate::OrbitRuntime;

/// How a claimed worker's call crosses the owner boundary. One exhaustive
/// classification, so a new tool family or verb must be placed here before the
/// crate compiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerRoute {
    /// A search: the scope check runs, then the call proceeds unchanged.
    SearchScope,
    /// A task show: the owner answers a `_worker_read` projection itself.
    TaskRead,
    /// An artifact read: the owner answers only while the claim is active.
    ArtifactRead,
    /// The call proceeds unchanged once the worker boundary checks pass.
    PassThrough,
    /// As `PassThrough`, after the binding validates the call's arguments.
    ValidatedPassThrough,
    /// A friction filing, translated into an owner claim mutation.
    FrictionAdd,
    /// A follow-up task filing, admitted only from the worker's own claim.
    TaskAdd,
    /// A task update or artifact put, translated into an owner claim mutation.
    TaskWrite,
    /// A write that belongs to the lifecycle boundary, never to a worker.
    Refuse,
}

/// The route of a worker's call, or `None` when the action is not a worker
/// coordination write or read and passes through untouched.
fn worker_route(action: OrbitBuiltinAction) -> Option<WorkerRoute> {
    match action {
        // Families that never cross the worker boundary, whatever the verb.
        OrbitBuiltinAction::Attention(_)
        | OrbitBuiltinAction::CommandExec
        | OrbitBuiltinAction::Decision(_)
        | OrbitBuiltinAction::DesktopRead
        | OrbitBuiltinAction::DesktopAutomation
        | OrbitBuiltinAction::DesktopTaskSnapshot
        | OrbitBuiltinAction::DesktopTaskWrite
        | OrbitBuiltinAction::Experiment(_)
        | OrbitBuiltinAction::PipelineInvoke
        | OrbitBuiltinAction::PipelineWait
        | OrbitBuiltinAction::Notice(_)
        | OrbitBuiltinAction::Resident(_)
        | OrbitBuiltinAction::Evolution(_)
        | OrbitBuiltinAction::Memory(_) => None,
        OrbitBuiltinAction::Drain(verb) => match verb {
            DrainVerb::Probe
            | DrainVerb::ReceiptLookup
            | DrainVerb::Pull
            | DrainVerb::ClaimBind
            | DrainVerb::ClaimSettle
            | DrainVerb::ClaimEffort
            | DrainVerb::Claims => None,
        },
        OrbitBuiltinAction::WorkspaceClaim(verb) => match verb {
            WorkspaceClaimVerb::Acquire
            | WorkspaceClaimVerb::Release
            | WorkspaceClaimVerb::Show => None,
        },
        OrbitBuiltinAction::Codebase(verb) => match verb {
            CodebaseVerb::Analytics | CodebaseVerb::Show | CodebaseVerb::Related => None,
        },
        OrbitBuiltinAction::Agents(verb) => match verb {
            AgentsVerb::Scorecard | AgentsVerb::Show | AgentsVerb::CiFailures => None,
        },
        // `ClearStall` is not routed here: the operator-only authorization
        // catalog entry is what keeps a worker from it.
        OrbitBuiltinAction::Automation(verb) => match verb {
            AutomationVerb::Explain | AutomationVerb::ClearStall => None,
        },
        OrbitBuiltinAction::Workflow(verb) => match verb {
            WorkflowVerb::Auto
            | WorkflowVerb::Land
            | WorkflowVerb::RunList
            | WorkflowVerb::RunResume
            | WorkflowVerb::RunObserve
            | WorkflowVerb::RunShow
            | WorkflowVerb::Ship => None,
        },
        OrbitBuiltinAction::Search(verb) => match verb {
            SearchVerb::Query => Some(WorkerRoute::SearchScope),
        },
        OrbitBuiltinAction::Friction(verb) => Some(match verb {
            FrictionVerb::Add => WorkerRoute::FrictionAdd,
            FrictionVerb::List | FrictionVerb::Show | FrictionVerb::Stats | FrictionVerb::Tags => {
                WorkerRoute::PassThrough
            }
            FrictionVerb::Update | FrictionVerb::Resolve | FrictionVerb::Rehome => {
                WorkerRoute::Refuse
            }
        }),
        OrbitBuiltinAction::Task(verb) => Some(match verb {
            // [ORB-14260] A claimed worker files follow-up work as ordinary
            // creation, but only work spawned from its own claimed task, and only
            // while the owner still holds that claim as active.
            TaskVerb::Add => WorkerRoute::TaskAdd,
            TaskVerb::Update | TaskVerb::ArtifactPut => WorkerRoute::TaskWrite,
            TaskVerb::Show => WorkerRoute::TaskRead,
            TaskVerb::ArtifactGet => WorkerRoute::ArtifactRead,
            // Lint can restore context, so it is not an unconditional read.
            // Resident admission precedes worker handling and refuses that
            // mode before either the named task or sweep can write.
            TaskVerb::Lint | TaskVerb::List | TaskVerb::Eligible | TaskVerb::LocksList => {
                WorkerRoute::PassThrough
            }
            TaskVerb::LocksReserve
            | TaskVerb::LocksRelease
            | TaskVerb::ReviewReset
            | TaskVerb::Repilot
            | TaskVerb::Requeue
            | TaskVerb::BlockTriage
            | TaskVerb::ReconcileReview
            | TaskVerb::RetryHandoff
            | TaskVerb::RecoverClaim
            | TaskVerb::RevokeHandoff
            | TaskVerb::Reject
            | TaskVerb::Delete => WorkerRoute::Refuse,
        }),
        // These writes use the owner's checkout-backed definition root. They
        // must pass the worker destination check before ordinary CRUD.
        OrbitBuiltinAction::AutoTask(verb) => match verb {
            AutoTaskVerb::Add | AutoTaskVerb::Update | AutoTaskVerb::Toggle => {
                Some(WorkerRoute::ValidatedPassThrough)
            }
            AutoTaskVerb::Delete
            | AutoTaskVerb::Restore
            | AutoTaskVerb::Recover
            | AutoTaskVerb::Reset => Some(WorkerRoute::Refuse),
            AutoTaskVerb::List | AutoTaskVerb::Show | AutoTaskVerb::Mint => None,
        },
    }
}

pub(crate) fn execute(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    action: OrbitBuiltinAction,
    input: &Value,
    model: Option<&str>,
) -> Result<Option<Value>, OrbitError> {
    let Some(route) = worker_route(action) else {
        return Ok(None);
    };
    crate::runtime::check_worker_host_input(input, session.worker_host_call)?;
    let Some(binding) = &session.worker_invocation else {
        return Ok(None);
    };
    binding.validate()?;
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
    match route {
        WorkerRoute::SearchScope => {
            crate::runtime::check_worker_search_scope(input)?;
            Ok(None)
        }
        WorkerRoute::TaskRead => match input.get("_worker_read").and_then(Value::as_str) {
            Some(projection) => worker_read(runtime, input, binding, projection).map(Some),
            None => Ok(None),
        },
        WorkerRoute::ArtifactRead => worker_artifact_read(runtime, session, binding, input),
        WorkerRoute::PassThrough => Ok(None),
        WorkerRoute::ValidatedPassThrough => {
            binding.validate_arguments(input)?;
            Ok(None)
        }
        WorkerRoute::FrictionAdd => worker_friction_add(runtime, session, binding, input, model),
        WorkerRoute::TaskAdd => worker_task_add(runtime, session, binding, input),
        WorkerRoute::TaskWrite => worker_update(runtime, session, binding, input).map(Some),
        WorkerRoute::Refuse => Err(OrbitError::PolicyDenied(
            "claimed worker operation requires its lifecycle boundary".into(),
        )),
    }
}

/// A claimed worker's friction filing as its owner's claim mutation.
fn worker_friction_add(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    binding: &WorkerInvocation,
    input: &Value,
    model: Option<&str>,
) -> Result<Option<Value>, OrbitError> {
    binding.validate_arguments(input)?;
    let parsed = FrictionVerb::Add.spec().parse_input(input.clone())?;
    let model = claimed_friction_model(runtime, binding, model);
    let (mut params, substitutions) = super::friction_tools::add_params(&parsed, Some(model))?;
    params.during_task = Some(binding.task_id.clone());
    let taxonomy = crate::runtime::friction::store_for(runtime)?
        .tags()?
        .into_iter()
        .collect();
    params.tags = orbit_store::contracts::normalize_friction_tags(params.tags, &taxonomy)?;
    apply(
        runtime,
        session,
        ClaimMutation::Friction(params),
        substitutions,
    )
    .map(Some)
}

/// Trusted attribution for a claimed worker's friction. The call's own
/// `model` field is never read: a session model the host asserted wins, then
/// the crew model the claim's bound run recorded, then the claimed task's
/// resolved crew, and finally the receiving (owner) process's resolved actor,
/// the documented fallback ordinary friction creation uses.
fn claimed_friction_model(
    runtime: &OrbitRuntime,
    binding: &WorkerInvocation,
    model: Option<&str>,
) -> String {
    let trimmed = |value: &str| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    };
    if let Some(model) = model.and_then(trimmed) {
        return model;
    }
    if let Ok(Some(run)) = runtime.get_job_run_backend(&binding.bound_run_id)
        && let Some(model) = run.crew_model.as_deref().and_then(trimmed)
    {
        return model;
    }
    if let Ok(task) = runtime.get_task(&binding.task_id)
        && let Ok(Some(projection)) = runtime.resolved_crew_projection(&task)
        && let Some(model) = trimmed(&projection.model)
    {
        return model;
    }
    runtime.actor().label.clone()
}

/// A claimed worker's follow-up filing: validated here, created by ordinary
/// task creation. [ORB-14792] A claimed review task's findings may also name
/// the task in this workspace that introduced each, as `regression_from`.
fn worker_task_add(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    binding: &WorkerInvocation,
    input: &Value,
) -> Result<Option<Value>, OrbitError> {
    binding.validate_arguments(input)?;
    if input.get(TASK_CLI_OPTIONS_PARAM).is_some() {
        return Err(OrbitError::PolicyDenied(
            "a claimed worker files work as ordinary creation, without the operator command's options"
                .into(),
        ));
    }
    let findings =
        orbit_types::workflow::files_regression_findings(&runtime.get_task(&binding.task_id)?.tags);
    let culprits = binding.validate_spawned_relations(input, findings)?;
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
    Ok(None)
}

/// An owner-side read projection of a claimed worker's task show.
fn worker_read(
    runtime: &OrbitRuntime,
    input: &Value,
    binding: &WorkerInvocation,
    projection: &str,
) -> Result<Value, OrbitError> {
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
        "task_metadata" => {
            return crate::application::task::metadata::owner_metadata_page(runtime, input);
        }
        // [ORB-22806] Regression-risk marks were removed. A follower still
        // on an earlier build asks for them at each prompt; answer "no
        // marks" until 2026-12-07 so it stays quiet, then delete the arm.
        "regression_risk" => Ok(Value::Null),
        // The owner's current crew-memory kill switch. A bool only: the
        // leaf already holds the captured block and must not receive
        // another copy of the notes, or this workspace's memory tables.
        "crew_memory_policy" => Ok(serde_json::json!({
            "enabled": runtime.current_crew_memory_enabled(),
        })),
        "status_index" => serde_json::to_value(runtime.task_status_index()?),
        "completion_by_complexity" => serde_json::to_value(
            runtime
                .task_completion_by_complexity()?
                .into_iter()
                .map(|row| (row.complexity, row.total, row.by_status))
                .collect::<Vec<_>>(),
        ),
        "complexity_by_id" => serde_json::to_value(runtime.task_complexity_by_id()?),
        "terminal_transitions" => serde_json::to_value(
            runtime.task_terminal_transitions(optional_field(input, "since")?)?,
        ),
        "defect_filings" => {
            serde_json::to_value(runtime.task_defect_filings(optional_field(input, "since")?)?)
        }
        "task" => serde_json::to_value(runtime.get_task(id)?),
        // Lightweight row for a show whose canonical bundle read failed.
        // `null` when the id has no settled bundle.
        "listed_task" => match runtime.get_listed_task_row(id)? {
            Some(row) => serde_json::to_value(row),
            None => Ok(Value::Null),
        },
        "artifacts" => serde_json::to_value(runtime.get_task_artifacts(id)?),
        "manifest" => serde_json::to_value(runtime.get_task_artifact_manifest(id)?),
        // Compatibility from 2026-11-09: pre-artifact followers decode
        // inline pilot audits. Keep this projection for at least 30 days
        // and until no supported follower needs the old receipt body.
        "comments" => serde_json::to_value(
            if input["pilot_artifacts"] == true && id == binding.task_id {
                runtime.get_task_comments(id)?
            } else {
                runtime.get_task_pilot_comments(id)?
            },
        ),
        "history" => serde_json::to_value(runtime.get_task_history(id)?),
        "dependency_history" => serde_json::to_value(runtime.dependency_history(id)?),
        // Mid-run steering's records for this worker's own claim, never
        // another task's: its promotion gate certifies only what the
        // owner holds.
        "steering" => {
            if id != binding.task_id {
                return Err(OrbitError::PolicyDenied(
                    "worker task binding mismatch".into(),
                ));
            }
            serde_json::to_value(runtime.worker_claim_steering(&worker_auth(binding))?)
        }
        "dependency" => match runtime.resolve_dependency_task(id)? {
            orbit_store::RegisteredTaskResolution::Resolved(task) => {
                serde_json::to_value(Some(task))
            }
            _ => serde_json::to_value(Option::<orbit_types::task::Task>::None),
        },
        _ => {
            return Err(OrbitError::InvalidInput(
                "unknown worker read projection".into(),
            ));
        }
    }
    .map_err(|error| OrbitError::Store(error.to_string()))?;
    Ok(value)
}

/// A claimed worker's artifact read: the owner answers only the worker's own
/// task, and only while its claim is active.
fn worker_artifact_read(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    binding: &WorkerInvocation,
    input: &Value,
) -> Result<Option<Value>, OrbitError> {
    require_active_claim(runtime, session)?;
    let Some(projection) = input.get("_worker_read").and_then(Value::as_str) else {
        return Ok(None);
    };
    if projection != "artifact" {
        return Err(OrbitError::InvalidInput(
            "unknown worker artifact read projection".into(),
        ));
    }
    if input.get("id").and_then(Value::as_str) != Some(binding.task_id.as_str()) {
        return Err(OrbitError::PolicyDenied(
            "worker task binding mismatch".into(),
        ));
    }
    let mut request = input.clone();
    if let Some(object) = request.as_object_mut() {
        object.remove("_worker_read");
    }
    let parsed = TaskVerb::ArtifactGet.spec().parse_input(request)?;
    let artifact = runtime.get_task_artifact_inline(
        &parsed.required_string("id")?,
        &parsed.required_string("path")?,
    )?;
    serde_json::to_value(artifact)
        .map(Some)
        .map_err(|error| OrbitError::Store(error.to_string()))
}

/// A claimed worker's task update, or an artifact put, as its owner's claim
/// mutation.
fn worker_update(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    binding: &WorkerInvocation,
    input: &Value,
) -> Result<Value, OrbitError> {
    binding.validate_arguments(input)?;
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
    // The host's own record of what a prompt it built carried; agent tool
    // calls cannot reach this channel (`check_worker_host_input`).
    if let Some(record) = input.get("_worker_steering") {
        let record: ClaimSteeringRecord = serde_json::from_value(record.clone())
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        return apply(
            runtime,
            session,
            ClaimMutation::Steering(record),
            Vec::new(),
        );
    }
    if let Some(update) = input.get("_worker_update") {
        let update: ClaimWorkerUpdate = serde_json::from_value(update.clone())
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        if !update.evidence.artifacts.is_empty() {
            return Err(OrbitError::InvalidInput(
                "use the task artifact adapter for artifact payloads".into(),
            ));
        }
        return apply(runtime, session, ClaimMutation::Update(update), Vec::new());
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
    let mut artifacts = super::input::parse_artifacts(input)?;
    // A worker on its owner's machine reaches this boundary in
    // process, past any broker, so the reserved review namespace is
    // enforced here for every route. Only the host's own channel —
    // the review gate writing its records — is the system writer.
    if !session.worker_host_call {
        for artifact in &mut artifacts {
            crate::application::task::admit_agent_artifact(artifact)?;
        }
    }
    let mutation = ClaimMutation::Update(ClaimWorkerUpdate {
        plan: orbit_common::protocol::tool_input::optional_raw_string(input, "plan")?,
        context_files: orbit_common::protocol::tool_input::optional_csv_or_string_list_alias(
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
            comment: orbit_common::protocol::tool_input::optional_raw_string(input, "comment")?,
            artifacts,
            ..Default::default()
        },
        ..Default::default()
    });
    apply(runtime, session, mutation, Vec::new())
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
        .map_err(|error| match error.claim_refusal() {
            Some(ClaimRefusalKind::StaleClaim) => OrbitError::PolicyDenied(
                "stale_claim: the owner no longer holds this worker's claim as active (it was \
                 released, failed, revoked, landed or superseded, or is bound to another run), \
                 so the owner refuses its artifact reads as it refuses its writes. Do not retry \
                 or route around the owner; report the work incomplete and let the run end"
                    .into(),
            ),
            _ => error,
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
