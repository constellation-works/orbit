//! Captured admission snapshots: the drain coordinator and every child it
//! admits carry the same bounded subset of the grant [ORB-11332].
//!
//! The coordinator captures its snapshot once at submission. A child inherits
//! exactly that snapshot at the trusted admission path and cannot acquire more
//! by rereading newer configuration; the Store rechecks the grant, scope,
//! task claim, and leaf capacity inside the same transaction that creates
//! the child. Ordinary submissions that set the reserved key are refused.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_config::{CompletionPreference, OperationPolicy};
use orbit_store::contracts::ChildAdmissionAuthority;
use orbit_types::workflow::{
    CompletionPolicy, GrantAdmission, OPERATION_ADMISSION_KEY, OperationAdmission, OperationGrant,
};
use serde_json::Value;

use super::captured_policy;
use crate::OrbitRuntime;
use crate::application::job::PipelineInvokeResult;
use crate::application::workflow::{AUTO_WORKFLOW_ALIAS, find_workflow};

/// The leaf job whose live runs count against the leaf ceiling.
const LEAF_JOB_NAME: &str = "task_auto_pipeline";
/// The epic job; its root task is scope-checked but not counted as a leaf.
const EPIC_JOB_NAME: &str = "epic_pipeline";

/// One request to start a drain window under a grant.
#[derive(Debug, Clone)]
pub struct OperationDrainRequest<'a> {
    /// The grant to bind; `None` selects the workspace's active grant.
    pub grant_id: Option<&'a str>,
    /// Requested window; capped at the grant's remaining time.
    pub for_seconds: Option<u64>,
    /// Requested leaf ceiling; may only narrow the grant's captured ceiling.
    pub max_active_leaf_runs: Option<u32>,
    pub allowed_crews: &'a [String],
    pub complexity_crews: &'a orbit_config::ComplexityCrewPools,
    pub actor: Option<&'a str>,
    pub claim_token: Option<&'a str>,
}

/// The submitted drain and what it captured.
#[derive(Debug, Clone)]
pub struct OperationDrainResult {
    pub invoke: PipelineInvokeResult,
    pub admission: OperationAdmission,
    pub window_seconds: u64,
    pub leaf_ceiling: u32,
}

impl OrbitRuntime {
    /// Start a bounded drain whose every admission is bound to a grant.
    ///
    /// The window is the intersection of the request and the grant's
    /// remaining time; the ceiling is the intersection of the request, the
    /// grant's captured ceiling, and the job's hard limit; completion is the
    /// captured effective completion and needs the grant's complete right.
    pub fn submit_operation_drain(
        &self,
        request: OperationDrainRequest<'_>,
    ) -> Result<OperationDrainResult, OrbitError> {
        self.require_workspace_claim("orbit.workflow.auto", request.claim_token)?;
        let grant = match request.grant_id {
            Some(id) => self.operation_grant(id)?,
            None => self.active_operation_grant()?.ok_or_else(|| {
                OrbitError::InvalidInput(
                    "this workspace has no active operation grant; enable one first".to_string(),
                )
            })?,
        };
        let now = Utc::now();
        let admission_state = grant.admission(now);
        if !admission_state.admits() {
            return Err(OrbitError::InvalidInput(format!(
                "operation grant '{}' is {}; enable a replacement window",
                grant.id,
                admission_state.reason()
            )));
        }
        let policy = captured_policy(&grant)?;
        let remaining = grant.remaining_seconds(now);
        let window_seconds = request
            .for_seconds
            .map_or(remaining, |requested| requested.min(remaining))
            .max(1);
        let hard_limit = self.leaf_run_hard_limit()?;
        let leaf_ceiling = grant
            .limits
            .leaf_ceiling
            .min(request.max_active_leaf_runs.unwrap_or(u32::MAX))
            .min(hard_limit)
            .max(1);
        let admission = admission_snapshot(&grant, &policy, leaf_ceiling);
        let completion = if admission.completion == "done" {
            CompletionPolicy::Done
        } else {
            CompletionPolicy::Review
        };

        let workflow = find_workflow(AUTO_WORKFLOW_ALIAS)
            .ok_or_else(|| OrbitError::InvalidInput("unknown workflow 'auto'".to_string()))?;
        let mut input = crate::application::job::pipeline::workspace_auto_run_input(
            Some(window_seconds),
            Some(leaf_ceiling),
            completion,
            &self.canonical_allowed_crews(request.allowed_crews)?,
        )?;
        Self::set_auto_crew_overrides(&mut input, request.complexity_crews);
        if let Some(object) = input.as_object_mut() {
            object.insert(
                OPERATION_ADMISSION_KEY.to_string(),
                serde_json::to_value(&admission).map_err(|error| {
                    OrbitError::Execution(format!("serialize operation admission: {error}"))
                })?,
            );
        }
        let invoke =
            self.submit_operation_bound_pipeline_run(workflow.job_id, input, request.actor)?;
        Ok(OperationDrainResult {
            invoke,
            admission,
            window_seconds,
            leaf_ceiling,
        })
    }
}

/// The bounded subset of a grant a run carries.
pub(crate) fn admission_snapshot(
    grant: &OperationGrant,
    policy: &OperationPolicy,
    leaf_ceiling: u32,
) -> OperationAdmission {
    let (capped, _) = policy.capped_completion();
    let completion = if grant.rights.complete && capped == CompletionPreference::Done {
        "done"
    } else {
        "review"
    };
    let mut limits = grant.limits;
    limits.leaf_ceiling = leaf_ceiling;
    OperationAdmission {
        grant_id: grant.id.clone(),
        grant_revision: grant.revision,
        policy_version: grant.policy_version,
        expires_at: grant.expires_at,
        completion: completion.to_string(),
        limits,
    }
}

/// The snapshot the executing coordinator was admitted under, read from its
/// own persisted run state through the engine-injected `run_id`. Absent for
/// every drain that predates operation mode or was started without a grant.
pub(crate) fn live_admission(runtime: &OrbitRuntime, input: &Value) -> Option<OperationAdmission> {
    let run_id = input
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let state = runtime.read_run_state(run_id).ok().flatten()?;
    OperationAdmission::from_run_input(&state.initial_input)
        .ok()
        .flatten()
}

/// The grant-bound facts for one child admission, derived from the parent's
/// captured snapshot. `None` when the parent carries none.
pub(crate) fn child_admission_authority(
    runtime: &OrbitRuntime,
    parent_run_id: &str,
    job_name: &str,
    child_input: &Value,
) -> Result<Option<(OperationAdmission, ChildAdmissionAuthority)>, OrbitError> {
    let Some(parent) = runtime.get_job_run_backend(parent_run_id)? else {
        return Ok(None);
    };
    let Some(snapshot) = parent
        .input
        .as_ref()
        .map(OperationAdmission::from_run_input)
        .transpose()
        .map_err(OrbitError::InvalidInput)?
        .flatten()
    else {
        return Ok(None);
    };

    let is_leaf = job_name == LEAF_JOB_NAME;
    let task_id = if is_leaf {
        child_input
            .get("task_ids")
            .and_then(Value::as_array)
            .and_then(|ids| ids.first())
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    } else if job_name == EPIC_JOB_NAME {
        child_input
            .get("epic_task_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    } else {
        None
    };
    // A live operator adjustment on the coordinator narrows the captured
    // ceiling but can never widen it past the grant.
    let leaf_ceiling = is_leaf.then(|| {
        let live = runtime
            .read_run_state(parent_run_id)
            .ok()
            .flatten()
            .and_then(|state| state.drain_worker_limit)
            .map(|limit| limit.max_active_leaf_runs);
        snapshot
            .limits
            .leaf_ceiling
            .min(live.unwrap_or(u32::MAX))
            .max(1)
    });
    let authority = ChildAdmissionAuthority {
        grant_id: snapshot.grant_id.clone(),
        grant_revision: snapshot.grant_revision,
        task_id,
        leaf_ceiling,
        now: Utc::now(),
    };
    Ok(Some((snapshot, authority)))
}

/// Install the parent's snapshot on a child input, replacing anything the
/// caller supplied. A child never widens what its parent captured.
pub(crate) fn inherit_child_admission(
    input: &mut Value,
    parent: &OperationAdmission,
) -> Result<(), OrbitError> {
    let object = input.as_object_mut().ok_or_else(|| {
        OrbitError::InvalidInput("pipeline run input must be a JSON object".to_string())
    })?;
    object.insert(
        OPERATION_ADMISSION_KEY.to_string(),
        serde_json::to_value(parent).map_err(|error| {
            OrbitError::Execution(format!("serialize operation admission: {error}"))
        })?,
    );
    Ok(())
}

/// The refusal for ordinary run input that set the reserved admission key.
pub(crate) fn reserved_operation_key_error(job_name: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "run input for job '{job_name}' set the reserved `{OPERATION_ADMISSION_KEY}` field; an \
         operation-mode admission is captured by `orbit operation` enablement and the drain it \
         starts, and cannot be requested through ordinary job input"
    ))
}

/// Whether a grant still admits new work, for the drain classifier and window.
pub(crate) fn admission_state(
    runtime: &OrbitRuntime,
    admission: &OperationAdmission,
) -> Result<(OperationGrant, GrantAdmission), OrbitError> {
    // A revision only moves through stop or revocation, so the live grant's
    // own admission answer already names what happened.
    let grant = runtime.operation_grant(&admission.grant_id)?;
    let state = grant.admission(Utc::now());
    Ok((grant, state))
}
