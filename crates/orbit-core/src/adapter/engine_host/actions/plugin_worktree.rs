//! Authenticated single-task worktree binding for deterministic `plugin.tool_call`
//! delivery steps.
//!
//! A mutating `plugin.tool_call` inside a `spec.task_delivery` job — or any
//! `plugin.tool_call` step that passes `workspace_path` — must bind to the
//! running job's single admitted task and authenticated `worktree_setup`
//! checkout before the plugin backend is spawned. Missing, mismatched, unowned,
//! symlinked, primary-checkout, or multi-task bindings fail closed before any
//! tool invocation.
//!
//! Existing `plugin.tool_call` invocations outside deterministic task delivery
//! (and read-only delivery steps that omit `workspace_path`) keep their
//! existing primary-checkout routing.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use orbit_common::fs::git::run_git;
use orbit_engine::DispatchError;
use orbit_tools::ToolContext;
use orbit_tools::plugin::PluginToolBinding;
use orbit_types::plugin::PluginExecutionKind;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{
    ActivityV2Spec, JobRun, JobRunState, JobV2, JobV2StepBody, PipelineState, SuccessfulCheckpoint,
};
use serde_json::Value;

use crate::OrbitRuntime;

/// Maximum retry-parent hops followed when a resumed delivery run reuses an
/// earlier run's `worktree_setup` checkpoint.
const MAX_RETRY_ANCESTOR_HOPS: usize = 64;

/// Host-authenticated delivery worktree and singleton task identity for one
/// deterministic `plugin.tool_call` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PluginWorktreeBinding {
    pub(super) worktree_root: PathBuf,
    pub(super) run_id: String,
    pub(super) task_id: String,
}

/// Authenticate and resolve the admitted single-task worktree for a
/// deterministic `plugin.tool_call`, or return `Ok(None)` when the call stays
/// on its existing primary-checkout routing.
///
/// Fail-closed (STD-02 §R31, STD-05 §R1/§R6/§R7): every integrity, ownership,
/// and containment check runs before the plugin backend is invoked.
pub(super) fn resolve_plugin_delivery_worktree(
    runtime: &OrbitRuntime,
    action: &str,
    config: &Value,
    input: &Value,
    tool_context: &ToolContext,
    binding: &PluginToolBinding,
) -> Result<Option<PluginWorktreeBinding>, DispatchError> {
    let run_id = host_attested_run_id(action, tool_context)?;
    let requested_workspace_path = extract_requested_workspace_path(action, config, input)?;

    let stored_run = match run_id {
        Some(run_id) => runtime.get_job_run_backend(run_id).map_err(|error| {
            DispatchError::action_failed(
                action,
                format!("read job run '{run_id}' for plugin worktree binding: {error}"),
            )
        })?,
        None => None,
    };
    let job_spec = match stored_run.as_ref() {
        Some(run) => {
            let (_, spec) = runtime.resolve_run_definition(run).map_err(|error| {
                DispatchError::action_failed(
                    action,
                    format!(
                        "resolve job definition '{}' for run '{}': {error}",
                        run.job_id, run.run_id
                    ),
                )
            })?;
            Some(spec)
        }
        None => None,
    };

    let is_task_delivery = job_spec.as_ref().is_some_and(JobV2::holds_task_delivery);
    let is_mutating = match binding.execution_kind {
        PluginExecutionKind::Mutating => true,
        PluginExecutionKind::ReadOnly => false,
    };
    if !(is_task_delivery && is_mutating) && requested_workspace_path.is_none() {
        return Ok(None);
    }

    let Some(run_id) = run_id else {
        return Err(DispatchError::action_failed(
            action,
            "plugin delivery worktree binding requires a host-attested job run; \
             refusing to invoke plugin without an owning run",
        ));
    };
    let Some(run) = stored_run else {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "job run '{run_id}' was not found in this workspace; refusing to bind \
                 plugin call to a worktree"
            ),
        ));
    };
    if run.state != JobRunState::Running {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "job run '{run_id}' is in state '{}', expected 'running' for a bound \
                 plugin delivery call",
                run.state
            ),
        ));
    }
    let Some(mut job_spec) = job_spec else {
        return Err(DispatchError::action_failed(
            action,
            format!("job definition for run '{run_id}' could not be loaded"),
        ));
    };
    if !job_spec.holds_task_delivery() {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "job '{}' does not declare `spec.task_delivery`; `plugin.tool_call` \
                 worktree binding is permitted only inside a task-delivery job",
                run.job_id
            ),
        ));
    }
    let Some(requested_workspace_path) = requested_workspace_path else {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "mutating plugin delivery call in job '{}' (run '{run_id}') requires an \
                 explicit `workspace_path` bound to this run's `worktree_setup` checkout",
                run.job_id
            ),
        ));
    };

    let step_task_id = extract_singleton_step_task_id(action, config, input)?;
    let run_task_id = extract_singleton_run_task_id(action, &run)?;
    if step_task_id != run_task_id {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "plugin delivery step task '{step_task_id}' does not match delivery run \
                 '{run_id}' task '{run_task_id}'"
            ),
        ));
    }

    let state = runtime
        .read_run_state(&run.run_id)
        .map_err(|error| {
            DispatchError::action_failed(
                action,
                format!("read pipeline state for run '{run_id}': {error}"),
            )
        })?
        .ok_or_else(|| {
            DispatchError::action_failed(
                action,
                format!(
                    "delivery run '{run_id}' has no persisted pipeline state; \
                     run `worktree_setup` before invoking a mutating plugin tool"
                ),
            )
        })?;
    if state.run_id != run.run_id || state.job_id != run.job_id {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "pipeline state identity ({}/{}) does not match delivery run ({}/{})",
                state.run_id, state.job_id, run.run_id, run.job_id
            ),
        ));
    }

    resolve_job_activities(runtime, action, &mut job_spec)?;
    let checkpoint = authenticated_worktree_checkpoint(action, &run.run_id, &job_spec, &state)?;
    let owner_run =
        authenticate_checkpoint_owner(runtime, action, &run, &run_task_id, &checkpoint.job_run_id)?;

    if let Some(step_job_run_id) = extract_optional_job_run_id(action, config, input)?
        && step_job_run_id != owner_run.run_id
    {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "plugin delivery step `job_run_id` '{step_job_run_id}' does not match \
                 authenticated worktree owner '{}'",
                owner_run.run_id
            ),
        ));
    }

    let worktree_root = authenticate_worktree_path(
        runtime,
        action,
        &owner_run,
        &job_spec,
        &state,
        &checkpoint,
        &requested_workspace_path,
    )?;

    verify_admitted_singleton_task(runtime, action, &owner_run.run_id, &step_task_id)?;

    Ok(Some(PluginWorktreeBinding {
        worktree_root,
        run_id: run.run_id,
        task_id: step_task_id,
    }))
}

struct AuthenticatedWorktreeCheckpoint {
    workspace_path: String,
    job_run_id: String,
    step_index: usize,
    step_input: Option<Value>,
    input_schema: Option<Value>,
}

fn host_attested_run_id<'a>(
    action: &str,
    tool_context: &'a ToolContext,
) -> Result<Option<&'a str>, DispatchError> {
    let binding_run = tool_context
        .activity_binding
        .as_ref()
        .map(|binding| binding.job_run_id.trim())
        .filter(|value| !value.is_empty());
    let reservation_run = tool_context
        .reservation_owner
        .as_ref()
        .map(|owner| owner.owner_run_id.trim())
        .filter(|value| !value.is_empty());
    match (binding_run, reservation_run) {
        (Some(left), Some(right)) if left != right => Err(DispatchError::action_failed(
            action,
            format!(
                "host-attested run identity disagrees between activity binding '{left}' \
                 and reservation owner '{right}'"
            ),
        )),
        (Some(run_id), _) | (None, Some(run_id)) => Ok(Some(run_id)),
        (None, None) => Ok(None),
    }
}

fn extract_requested_workspace_path(
    action: &str,
    config: &Value,
    input: &Value,
) -> Result<Option<String>, DispatchError> {
    let from_input = parse_optional_non_empty_string(action, input, "workspace_path")?;
    let from_config = parse_optional_non_empty_string(action, config, "workspace_path")?;
    match (from_input, from_config) {
        (Some(left), Some(right)) if left != right => Err(DispatchError::action_failed(
            action,
            format!(
                "conflicting `workspace_path` values between step input '{left}' and \
                 activity config '{right}'"
            ),
        )),
        (Some(path), _) | (None, Some(path)) => Ok(Some(path)),
        (None, None) => Ok(None),
    }
}

fn extract_optional_job_run_id(
    action: &str,
    config: &Value,
    input: &Value,
) -> Result<Option<String>, DispatchError> {
    let from_input = parse_optional_non_empty_string(action, input, "job_run_id")?;
    let from_config = parse_optional_non_empty_string(action, config, "job_run_id")?;
    match (from_input, from_config) {
        (Some(left), Some(right)) if left != right => Err(DispatchError::action_failed(
            action,
            format!(
                "conflicting `job_run_id` values between step input '{left}' and \
                 activity config '{right}'"
            ),
        )),
        (Some(run_id), _) | (None, Some(run_id)) => Ok(Some(run_id)),
        (None, None) => Ok(None),
    }
}

fn parse_optional_non_empty_string(
    action: &str,
    source: &Value,
    field: &str,
) -> Result<Option<String>, DispatchError> {
    let Some(value) = source.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(text) = value.as_str() else {
        return Err(DispatchError::action_failed(
            action,
            format!("`{field}` must be a non-empty string"),
        ));
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(DispatchError::action_failed(
            action,
            format!("`{field}` must be a non-empty string"),
        ));
    }
    Ok(Some(trimmed.to_string()))
}

fn extract_singleton_step_task_id(
    action: &str,
    config: &Value,
    input: &Value,
) -> Result<String, DispatchError> {
    let mut candidates = Vec::new();
    for (label, source) in [("step input", input), ("activity config", config)] {
        if let Some(task_id) = parse_optional_non_empty_string(action, source, "task_id")? {
            candidates.push((format!("{label}.task_id"), task_id));
        }
        if let Some(task_ids) = source.get("task_ids").filter(|value| !value.is_null()) {
            let task_id =
                parse_singleton_task_ids_array(action, task_ids, &format!("{label}.task_ids"))?;
            candidates.push((format!("{label}.task_ids"), task_id));
        }
        if let Some(task_obj) = source.get("task").filter(|value| !value.is_null())
            && let Some(task_id) = parse_optional_non_empty_string(action, task_obj, "id")?
        {
            candidates.push((format!("{label}.task.id"), task_id));
        }
    }

    let Some((first_label, first_task_id)) = candidates.first() else {
        return Err(DispatchError::action_failed(
            action,
            "missing task binding for plugin delivery call: supply `task_id` or a \
             single-element `task_ids` array matching the admitted task",
        ));
    };
    for (label, task_id) in candidates.iter().skip(1) {
        if task_id != first_task_id {
            return Err(DispatchError::action_failed(
                action,
                format!(
                    "mismatched task binding: `{first_label}` names '{first_task_id}' \
                     while `{label}` names '{task_id}'"
                ),
            ));
        }
    }
    Ok(first_task_id.clone())
}

fn extract_singleton_run_task_id(action: &str, run: &JobRun) -> Result<String, DispatchError> {
    let Some(input) = run.input.as_ref() else {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "delivery run '{}' carries no input task binding; expected exactly one task",
                run.run_id
            ),
        ));
    };
    let mut candidates = Vec::new();
    if let Some(task_id) = parse_optional_non_empty_string(action, input, "task_id")? {
        candidates.push(("run.input.task_id", task_id));
    }
    if let Some(task_ids) = input.get("task_ids").filter(|value| !value.is_null()) {
        let task_id = parse_singleton_task_ids_array(action, task_ids, "run.input.task_ids")?;
        candidates.push(("run.input.task_ids", task_id));
    }
    let Some((first_label, first_task_id)) = candidates.first() else {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "delivery run '{}' carries no task ID in its input; expected exactly one task",
                run.run_id
            ),
        ));
    };
    for (label, task_id) in candidates.iter().skip(1) {
        if task_id != first_task_id {
            return Err(DispatchError::action_failed(
                action,
                format!(
                    "delivery run '{}' has conflicting task IDs: `{first_label}` is \
                     '{first_task_id}' while `{label}` is '{task_id}'",
                    run.run_id
                ),
            ));
        }
    }
    Ok(first_task_id.clone())
}

fn parse_singleton_task_ids_array(
    action: &str,
    value: &Value,
    field: &str,
) -> Result<String, DispatchError> {
    let Some(items) = value.as_array() else {
        return Err(DispatchError::action_failed(
            action,
            format!("`{field}` must be an array containing exactly one task ID"),
        ));
    };
    let mut parsed = Vec::with_capacity(items.len());
    for item in items {
        let Some(text) = item.as_str().map(str::trim).filter(|text| !text.is_empty()) else {
            return Err(DispatchError::action_failed(
                action,
                format!("`{field}` entries must be non-empty strings"),
            ));
        };
        parsed.push(text.to_string());
    }
    match parsed.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err(DispatchError::action_failed(
            action,
            format!(
                "`{field}` is empty; a bound plugin delivery call requires exactly one task ID"
            ),
        )),
        many => Err(DispatchError::action_failed(
            action,
            format!(
                "a bound plugin delivery call supports exactly one admitted task, got {} \
                 task IDs in `{field}`",
                many.len()
            ),
        )),
    }
}

fn resolve_job_activities(
    runtime: &OrbitRuntime,
    action: &str,
    job_spec: &mut JobV2,
) -> Result<(), DispatchError> {
    let catalog = runtime.v2_activity_catalog().map_err(|error| {
        DispatchError::action_failed(action, format!("load activity catalog: {error}"))
    })?;
    let scripts = runtime.v2_script_catalog().map_err(|error| {
        DispatchError::action_failed(action, format!("load script catalog: {error}"))
    })?;
    orbit_engine::resolve_job_catalog_refs_with_scripts_for_execution(job_spec, &catalog, &scripts)
        .map_err(|error| {
            DispatchError::action_failed(
                action,
                format!("resolve job activity catalog refs: {error}"),
            )
        })
}

fn authenticated_worktree_checkpoint(
    action: &str,
    run_id: &str,
    job_spec: &JobV2,
    state: &PipelineState,
) -> Result<AuthenticatedWorktreeCheckpoint, DispatchError> {
    let mut found = Vec::new();
    for (index, step) in job_spec.steps.iter().enumerate() {
        let JobV2StepBody::Target(target) = &step.body else {
            continue;
        };
        let ActivityV2Spec::Deterministic(spec) = &target.spec else {
            continue;
        };
        if spec.action != orbit_types::workflow::WORKTREE_SETUP_STEP_ID {
            continue;
        }
        let Ok(step_index) = u32::try_from(index) else {
            continue;
        };
        match state.successful_checkpoint(step_index, &step.id) {
            SuccessfulCheckpoint::Recorded(output) => {
                let workspace_path =
                    parse_optional_non_empty_string(action, output, "workspace_path")?.ok_or_else(
                        || {
                            DispatchError::action_failed(
                                action,
                                format!(
                                    "worktree_setup checkpoint for step '{}' in run '{run_id}' \
                                 is missing `workspace_path`",
                                    step.id
                                ),
                            )
                        },
                    )?;
                let job_run_id = parse_optional_non_empty_string(action, output, "job_run_id")?
                    .ok_or_else(|| {
                        DispatchError::action_failed(
                            action,
                            format!(
                                "worktree_setup checkpoint for step '{}' in run '{run_id}' \
                                 is missing `job_run_id`",
                                step.id
                            ),
                        )
                    })?;
                found.push(AuthenticatedWorktreeCheckpoint {
                    workspace_path,
                    job_run_id,
                    step_index: index,
                    step_input: target.default_input.clone(),
                    input_schema: target.input_schema_json.clone(),
                });
            }
            SuccessfulCheckpoint::Inconsistent => {
                return Err(DispatchError::action_failed(
                    action,
                    format!(
                        "worktree_setup checkpoint for step '{}' in run '{run_id}' is \
                         inconsistent between step output and pipeline state",
                        step.id
                    ),
                ));
            }
            SuccessfulCheckpoint::Absent => {}
        }
    }

    let count = found.len();
    let mut iter = found.into_iter();
    match (iter.next(), iter.next()) {
        (None, _) => Err(DispatchError::action_failed(
            action,
            format!(
                "delivery run '{run_id}' has no authenticated completed `worktree_setup` \
                 checkpoint; run `worktree_setup` before dispatching a mutating plugin tool"
            ),
        )),
        (Some(only), None) => Ok(only),
        (Some(_), Some(_)) => Err(DispatchError::action_failed(
            action,
            format!(
                "delivery run '{run_id}' has {count} completed `worktree_setup` \
                 checkpoints; refusing ambiguous worktree binding"
            ),
        )),
    }
}

fn authenticate_checkpoint_owner(
    runtime: &OrbitRuntime,
    action: &str,
    run: &JobRun,
    expected_task_id: &str,
    checkpoint_run_id: &str,
) -> Result<JobRun, DispatchError> {
    if checkpoint_run_id == run.run_id {
        return Ok(run.clone());
    }
    let mut visited = BTreeSet::from([run.run_id.clone()]);
    let mut current = run.clone();
    for _ in 0..MAX_RETRY_ANCESTOR_HOPS {
        let Some(parent_id) = current.retry_source_run_id.as_deref() else {
            break;
        };
        if !visited.insert(parent_id.to_string()) {
            return Err(DispatchError::action_failed(
                action,
                format!(
                    "retry lineage for run '{}' repeats ancestor '{parent_id}'",
                    run.run_id
                ),
            ));
        }
        let parent = runtime
            .get_job_run_backend(parent_id)
            .map_err(|error| {
                DispatchError::action_failed(
                    action,
                    format!("load retry ancestor '{parent_id}': {error}"),
                )
            })?
            .ok_or_else(|| {
                DispatchError::action_failed(
                    action,
                    format!(
                        "retry ancestor '{parent_id}' of run '{}' does not exist",
                        run.run_id
                    ),
                )
            })?;
        if parent.job_id != run.job_id {
            return Err(DispatchError::action_failed(
                action,
                format!(
                    "retry ancestor '{}' belongs to job '{}', expected '{}'",
                    parent.run_id, parent.job_id, run.job_id
                ),
            ));
        }
        let parent_task_id = extract_singleton_run_task_id(action, &parent)?;
        if parent_task_id != expected_task_id {
            return Err(DispatchError::action_failed(
                action,
                format!(
                    "retry ancestor '{}' delivered task '{parent_task_id}', expected \
                     '{expected_task_id}'",
                    parent.run_id
                ),
            ));
        }
        if parent.run_id == checkpoint_run_id {
            return Ok(parent);
        }
        current = parent;
    }

    Err(DispatchError::action_failed(
        action,
        format!(
            "worktree checkpoint owner '{checkpoint_run_id}' does not match delivery run \
             '{}' or its retry lineage",
            run.run_id
        ),
    ))
}

fn authenticate_worktree_path(
    runtime: &OrbitRuntime,
    action: &str,
    owner_run: &JobRun,
    job_spec: &JobV2,
    state: &PipelineState,
    checkpoint: &AuthenticatedWorktreeCheckpoint,
    requested_workspace_path: &str,
) -> Result<PathBuf, DispatchError> {
    let canonical_requested =
        canonicalize_non_symlink_worktree_dir(action, "workspace_path", requested_workspace_path)?;
    let canonical_checkpoint = canonicalize_non_symlink_worktree_dir(
        action,
        "worktree_setup checkpoint workspace_path",
        &checkpoint.workspace_path,
    )?;
    if canonical_requested != canonical_checkpoint {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "requested `workspace_path` '{}' resolves to '{}', which does not match \
                 the authenticated worktree '{}' for run '{}'",
                requested_workspace_path,
                canonical_requested.display(),
                canonical_checkpoint.display(),
                owner_run.run_id
            ),
        ));
    }

    let repo_root = &runtime.paths().repo_root;
    let canonical_repo_root = std::fs::canonicalize(repo_root).map_err(|error| {
        DispatchError::action_failed(
            action,
            format!(
                "canonicalize primary repository root '{}': {error}",
                repo_root.display()
            ),
        )
    })?;
    if canonical_requested == canonical_repo_root {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "refusing to bind plugin delivery call to the primary checkout '{}'",
                canonical_repo_root.display()
            ),
        ));
    }

    // Render only the identity field: other setup inputs can reference runtime
    // fields which are unnecessary for deriving the owned path, while
    // `branch_prefix` itself may depend on prior step outputs.
    let prefix_input = checkpoint.step_input.as_ref().map(|input| match input {
        Value::Object(fields) => serde_json::json!({
            "branch_prefix": fields.get("branch_prefix").cloned().unwrap_or(Value::Null)
        }),
        other => other.clone(),
    });
    let rendered = orbit_engine::activity_job::render_job_step_input_from_state(
        job_spec,
        owner_run.input.as_ref().unwrap_or(&Value::Null),
        state,
        checkpoint.step_index,
        prefix_input.as_ref(),
        checkpoint.input_schema.as_ref(),
    )
    .map_err(|error| {
        DispatchError::action_failed(
            action,
            format!(
                "render worktree setup branch prefix for run '{}': {error}",
                owner_run.run_id
            ),
        )
    })?;
    let mut run_for_derivation = owner_run.clone();
    let input = run_for_derivation
        .input
        .get_or_insert_with(|| serde_json::json!({}));
    if let Some(object) = input.as_object_mut() {
        object.remove("branch_prefix");
        if let Some(prefix) = rendered.get("branch_prefix") {
            object.insert("branch_prefix".to_string(), prefix.clone());
        }
    }
    let derived_paths =
        orbit_engine::run_worktree_paths(runtime.env(), repo_root, &run_for_derivation).map_err(
            |error| {
                DispatchError::action_failed(
                    action,
                    format!(
                        "derive owned worktree paths for run '{}': {error}",
                        owner_run.run_id
                    ),
                )
            },
        )?;
    let matched_derived = derived_paths.iter().find(|candidate| {
        std::fs::canonicalize(candidate).is_ok_and(|canonical| canonical == canonical_requested)
    });
    let Some(matched_derived) = matched_derived else {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "worktree '{}' is not owned by delivery run '{}'",
                canonical_requested.display(),
                owner_run.run_id
            ),
        ));
    };
    let managed_root = matched_derived.parent().ok_or_else(|| {
        DispatchError::action_failed(
            action,
            "managed worktree root has no parent directory".to_string(),
        )
    })?;
    let canonical_managed_root = std::fs::canonicalize(managed_root).map_err(|error| {
        DispatchError::action_failed(
            action,
            format!(
                "canonicalize managed worktree root '{}': {error}",
                managed_root.display()
            ),
        )
    })?;
    if canonical_requested.parent() != Some(canonical_managed_root.as_path()) {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "worktree '{}' is not a direct child of managed worktree root '{}'",
                canonical_requested.display(),
                canonical_managed_root.display()
            ),
        ));
    }

    verify_registered_linked_worktree(
        action,
        repo_root,
        &canonical_repo_root,
        &canonical_requested,
    )?;

    Ok(canonical_requested)
}

fn canonicalize_non_symlink_worktree_dir(
    action: &str,
    label: &str,
    raw: &str,
) -> Result<PathBuf, DispatchError> {
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(DispatchError::action_failed(
            action,
            format!("{label} '{raw}' must be an absolute path"),
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(DispatchError::action_failed(
            action,
            format!("{label} '{raw}' must not contain parent traversal (`..`)"),
        ));
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        DispatchError::action_failed(
            action,
            format!("{label} '{raw}' does not exist or cannot be inspected: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(DispatchError::action_failed(
            action,
            format!("{label} '{raw}' must not be a symbolic link"),
        ));
    }
    if !metadata.is_dir() {
        return Err(DispatchError::action_failed(
            action,
            format!("{label} '{raw}' must be a directory"),
        ));
    }
    std::fs::canonicalize(path).map_err(|error| {
        DispatchError::action_failed(action, format!("canonicalize {label} '{raw}': {error}"))
    })
}

fn verify_registered_linked_worktree(
    action: &str,
    repo_root: &Path,
    canonical_repo_root: &Path,
    canonical_worktree: &Path,
) -> Result<(), DispatchError> {
    let output = run_git(
        &orbit_common::Env::capture(),
        repo_root,
        &["worktree", "list", "--porcelain"],
    )
    .map_err(|error| {
        DispatchError::action_failed(
            action,
            format!(
                "inspect registered Git worktrees for '{}': {error}",
                repo_root.display()
            ),
        )
    })?;
    if !output.success {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "git worktree list failed for '{}': {}",
                repo_root.display(),
                output.stderr.trim()
            ),
        ));
    }
    let is_linked = output
        .stdout
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .filter_map(|recorded| std::fs::canonicalize(recorded).ok())
        .any(|registered| registered == canonical_worktree && registered != canonical_repo_root);
    if !is_linked {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "worktree '{}' is not a registered linked Git worktree of '{}'",
                canonical_worktree.display(),
                canonical_repo_root.display()
            ),
        ));
    }
    Ok(())
}

fn verify_admitted_singleton_task(
    runtime: &OrbitRuntime,
    action: &str,
    owner_run_id: &str,
    expected_task_id: &str,
) -> Result<(), DispatchError> {
    let admitted = runtime.list_run_tasks(owner_run_id).map_err(|error| {
        DispatchError::action_failed(
            action,
            format!("list tasks admitted to run '{owner_run_id}': {error}"),
        )
    })?;
    let [task] = admitted.as_slice() else {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "delivery run '{owner_run_id}' must have exactly one admitted task for a \
                 bound plugin call, found {}",
                admitted.len()
            ),
        ));
    };
    if task.id != expected_task_id {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "plugin delivery step task '{expected_task_id}' does not match admitted \
                 task '{}' on run '{owner_run_id}'",
                task.id
            ),
        ));
    }
    if task.status != TaskStatus::InProgress {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "task '{}' is in status '{}', expected 'in-progress' for an active \
                 plugin delivery call",
                task.id, task.status
            ),
        ));
    }
    if task.job_run_id.as_deref() != Some(owner_run_id) {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "task '{}' is bound to run {:?}, expected '{owner_run_id}'",
                task.id, task.job_run_id
            ),
        ));
    }
    Ok(())
}
