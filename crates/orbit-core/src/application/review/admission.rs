//! Captured review admission: the `review.timing` layer, minutes and crew a
//! delivery run carries in its immutable input [ORB-11333] [ORB-13992]
//! [ORB-14849].
//!
//! Resolution order at submission: a parent-authorized child inherits its
//! parent's snapshot exactly; every other delivery run resolves from
//! workspace configuration at that moment, strengthened by the run's
//! `--review` request, if any [ORB-16572]. The request can only add the layer
//! workspace policy does not apply: a matching layer keeps its policy
//! provenance but still requires a reviewer crew, and the other layer is
//! refused. Ordinary input naming the reserved key is refused, and a resume
//! carries its persisted input forward unchanged.
//! A `review.timing` other than `off` on `task_local_pipeline` is local-only
//! final delivery with no PR, and is refused outright. After-landing review is the `delivery-code-review`
//! auto-task, not an admission value, so it never reaches a snapshot.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_config::{PRE_LANDING_REVIEW_CREW_KEY, REVIEW_CREW_KEY};
use orbit_types::workflow::{
    REVIEW_ADMISSION_KEY, REVIEW_CONTRACT_VERSION, REVIEW_REQUEST_KEY, REVIEW_RUN_OPTION_SOURCE,
    ReviewAdmission, ReviewLayer, ReviewTiming,
};
use serde_json::Value;

use super::crew::pre_landing_review_crew_unset;
use super::{LOCAL_ROUTE_JOB, REVIEW_ADMITTED_JOBS};
use crate::OrbitRuntime;

/// Whether caller-shaped run input names the reserved review key.
fn declares_review_admission(input: &Value) -> bool {
    input
        .get(REVIEW_ADMISSION_KEY)
        .is_some_and(|value| !value.is_null())
}

/// Install the review admission a delivery submission carries, or refuse a
/// submission that tries to supply one. `parent_run_id` is set for the
/// parent-authorized child path; `resuming` keeps persisted input as is.
pub(crate) fn install_review_admission(
    runtime: &OrbitRuntime,
    job_name: &str,
    input: &mut Value,
    parent_run_id: Option<&str>,
    resuming: bool,
) -> Result<(), OrbitError> {
    if resuming {
        return Ok(());
    }
    let request = ReviewLayer::from_run_input(input)?;
    if !REVIEW_ADMITTED_JOBS.contains(&job_name) {
        if declares_review_admission(input) {
            return Err(reserved_review_key_error(job_name));
        }
        if request.is_some() {
            return Err(OrbitError::InvalidInput(format!(
                "job '{job_name}' captures no review admission, so its `{REVIEW_REQUEST_KEY}` \
                 cannot apply; request review on a delivery run (`orbit run ship` or \
                 `orbit run auto`)"
            )));
        }
        return Ok(());
    }
    if request.is_some() && job_name == crate::application::distributed::PULL_DRAIN_JOB {
        return Err(OrbitError::InvalidInput(
            pull_review_request_refusal().to_string(),
        ));
    }

    let inherited = match parent_run_id {
        Some(parent_run_id) => parent_review_admission(runtime, parent_run_id)?,
        None => None,
    };
    if inherited.is_none() && declares_review_admission(input) {
        return Err(reserved_review_key_error(job_name));
    }

    // A child takes its parent's admission whole, the parent's request
    // included, so its own input never re-resolves one.
    let admission = match inherited {
        Some(admission) => admission,
        None => resolve(runtime, request)?,
    };
    if job_name == LOCAL_ROUTE_JOB
        && let Some(layer) = ReviewLayer::from_timing(admission.timing)
    {
        return Err(OrbitError::InvalidInput(local_route_conflict(
            layer,
            &admission.timing_source,
        )));
    }

    let object = input.as_object_mut().ok_or_else(|| {
        OrbitError::InvalidInput("pipeline run input must be a JSON object".to_string())
    })?;
    object.insert(
        REVIEW_ADMISSION_KEY.to_string(),
        serde_json::to_value(&admission).map_err(|error| {
            OrbitError::Execution(format!("serialize review admission: {error}"))
        })?,
    );
    Ok(())
}

/// The snapshot a parent run persisted, if it carries one.
fn parent_review_admission(
    runtime: &OrbitRuntime,
    parent_run_id: &str,
) -> Result<Option<ReviewAdmission>, OrbitError> {
    let Some(parent) = runtime.get_job_run_backend(parent_run_id)? else {
        return Ok(None);
    };
    parent
        .input
        .as_ref()
        .map(ReviewAdmission::from_run_input)
        .transpose()
        .map_err(OrbitError::from)
        .map(Option::flatten)
}

/// The fail-closed sentence `install_review_admission` returns for
/// `review.timing = before_pr` on `task_local_pipeline`. Pre-dispatch surfaces quote
/// it so an operator sees one explanation before and after a spawn.
pub(crate) fn local_route_before_pr_refusal() -> &'static str {
    "review.timing = before_pr holds PR creation for a reviewer and has no meaning on the \
     local-only delivery route; ship through the PR route or set review.timing = off \
     for local delivery (after-landing review is the delivery-code-review auto-task)"
}

/// That refusal with the deciding layer inserted after the key, for doctor
/// and readiness. `source` is the operation-layer label (`global`,
/// `workspace`, or `built-in`).
pub(crate) fn local_route_before_pr_conflict(source: &str) -> String {
    with_source(
        local_route_before_pr_refusal(),
        ReviewLayer::BeforePr.config_setting(),
        source,
    )
}

/// The fail-closed sentence `install_review_admission` returns for
/// `review.timing = before_landing` on `task_local_pipeline` [ORB-14849]: a local
/// delivery opens no pull request to review before it lands.
pub(crate) fn local_route_before_landing_refusal() -> &'static str {
    "review.timing = before_landing reviews an open pull request before it lands and has no \
     meaning on the local-only delivery route, which opens none; ship through the PR route \
     or set review.timing = off for local delivery (after-landing review is the \
     delivery-code-review auto-task)"
}

/// [`local_route_before_landing_refusal`] with its deciding layer, for
/// doctor and readiness.
pub(crate) fn local_route_before_landing_conflict(source: &str) -> String {
    with_source(
        local_route_before_landing_refusal(),
        ReviewLayer::BeforeLanding.config_setting(),
        source,
    )
}

fn with_source(refusal: &str, key: &str, source: &str) -> String {
    format!("{key} ({source}){}", refusal.trim_start_matches(key))
}

/// The local-route refusal for a captured layer, worded for whichever layer
/// decided it: a run's `--review` option, or the configuration key with its
/// source.
pub(crate) fn local_route_conflict(layer: ReviewLayer, source: &str) -> String {
    if source == REVIEW_RUN_OPTION_SOURCE {
        return local_route_run_option_refusal(layer);
    }
    match layer {
        ReviewLayer::BeforePr => local_route_before_pr_conflict(source),
        ReviewLayer::BeforeLanding => local_route_before_landing_conflict(source),
    }
}

/// The refusal for a `--review` request on the local-only delivery route
/// [ORB-16572]: it opens no pull request for either layer to review.
pub(crate) fn local_route_run_option_refusal(layer: ReviewLayer) -> String {
    let meaning = match layer {
        ReviewLayer::BeforePr => {
            "holds PR creation for a reviewer and has no meaning on the local-only delivery route"
        }
        ReviewLayer::BeforeLanding => {
            "reviews an open pull request before it lands and has no meaning on the local-only \
             delivery route, which opens none"
        }
    };
    format!(
        "--review {} {meaning}; ship through the PR route or submit without --review \
         (after-landing review is the delivery-code-review auto-task)",
        layer.as_str()
    )
}

/// Why a pull drain refuses `--review` [ORB-16572].
pub fn pull_review_request_refusal() -> &'static str {
    "--review does not apply to a pull drain: the owner decides the review of the claims it \
     hands out, so request review on the owner's own run or in its workspace policy"
}

/// Record a run's `--review` request in its input, refusing it before any
/// run exists when it could not be admitted [ORB-16572]: on the local-only
/// route (`local_route`), against the other workspace layer, or without a
/// reviewer crew. Submission resolves the recorded request again.
pub(crate) fn admit_review_request(
    runtime: &OrbitRuntime,
    input: &mut Value,
    review: Option<ReviewLayer>,
    local_route: bool,
) -> Result<(), OrbitError> {
    let Some(layer) = review else {
        return Ok(());
    };
    if local_route {
        return Err(OrbitError::InvalidInput(local_route_run_option_refusal(
            layer,
        )));
    }
    resolve(runtime, Some(layer))?;
    let object = input.as_object_mut().ok_or_else(|| {
        OrbitError::InvalidInput("pipeline run input must be a JSON object".to_string())
    })?;
    object.insert(
        REVIEW_REQUEST_KEY.to_string(),
        Value::String(layer.as_str().to_string()),
    );
    Ok(())
}

/// The admission a run without a parent captures: workspace policy, with the
/// run's `--review` request added when policy applies no layer before
/// landing [ORB-16572]. The request never removes or swaps a layer. A matching
/// layer keeps policy provenance, and both matching and added layers need
/// the same reviewer pool a configured layer does:
/// `workflow.purpose_crews.pre_landing_review`, else
/// `workflow.purpose_crews.review`.
pub(crate) fn resolve(
    runtime: &OrbitRuntime,
    request: Option<ReviewLayer>,
) -> Result<ReviewAdmission, OrbitError> {
    let mut admission = snapshot(runtime)?;
    let Some(layer) = request else {
        return Ok(admission);
    };
    if admission.timing == layer.timing() {
        require_reviewer_crew(&admission, layer, Some(&admission.timing_source))?;
        return Ok(admission);
    }
    if let Some(applied) = ReviewLayer::from_timing(admission.timing) {
        return Err(OrbitError::InvalidInput(format!(
            "--review {} conflicts with {} ({}), which already applies {} review to this \
             workspace; there is one review layer before landing, and --review can only add \
             review where workspace policy applies none. Submit without --review to keep {}",
            layer.as_str(),
            applied.config_setting(),
            admission.timing_source,
            applied.as_str(),
            applied.as_str(),
        )));
    }
    require_reviewer_crew(&admission, layer, None)?;
    admission.timing = layer.timing();
    admission.timing_source = REVIEW_RUN_OPTION_SOURCE.to_string();
    Ok(admission)
}

/// Refuse a requested layer when neither a reviewer crew nor a reviewer pool
/// is configured. When workspace policy already applies the layer, name that
/// policy source and do not suggest removing `--review`.
fn require_reviewer_crew(
    admission: &ReviewAdmission,
    layer: ReviewLayer,
    applied_source: Option<&str>,
) -> Result<(), OrbitError> {
    if admission.crew.is_some() || !admission.crew_pool.is_empty() {
        return Ok(());
    }
    let unset = pre_landing_review_crew_unset();
    let message = match applied_source {
        Some(source) => format!(
            "{} ({source}) requires a reviewer crew, but {unset}; configure \
             {PRE_LANDING_REVIEW_CREW_KEY} or {REVIEW_CREW_KEY} (automatic review never inherits \
             the implementer's crew)",
            layer.config_setting()
        ),
        None => format!(
            "--review {} needs a reviewer crew, but {unset}; configure \
             {PRE_LANDING_REVIEW_CREW_KEY} or {REVIEW_CREW_KEY} (automatic review never inherits \
             the implementer's crew) or submit without --review",
            layer.as_str()
        ),
    };
    Err(OrbitError::InvalidInput(message))
}

/// Build the snapshot from the workspace's resolved review settings, keeping
/// each field's provenance so diagnostics can explain where it came from.
///
/// The reviewer pool is `workflow.purpose_crews.pre_landing_review`, else
/// `workflow.purpose_crews.review`, with `crew_source` the layer that set
/// the key it came from. A single review crew is captured as `crew`; a pool
/// is captured whole as `crew_pool`, and the gate chooses one member per
/// review [ORB-15195].
pub(crate) fn snapshot(runtime: &OrbitRuntime) -> Result<ReviewAdmission, OrbitError> {
    let policy = runtime.operation_policy();
    let mut crew_pool = runtime.pre_landing_review_crew_pool_members()?;
    let crew = match crew_pool.as_slice() {
        [only] => Some(only.name.clone()),
        _ => None,
    };
    if crew.is_some() {
        crew_pool.clear();
    }
    Ok(ReviewAdmission {
        contract_version: REVIEW_CONTRACT_VERSION,
        policy_version: policy.version,
        // Configuration never resolves both switches on.
        timing: if policy.review_before_pr.value {
            ReviewTiming::BeforePr
        } else if policy.review_before_landing.value {
            ReviewTiming::BeforeLanding
        } else {
            ReviewTiming::None
        },
        timing_source: if policy.review_before_landing.value && !policy.review_before_pr.value {
            policy.review_before_landing.source.label().to_string()
        } else {
            policy.review_before_pr.source.label().to_string()
        },
        crew,
        crew_source: policy.pre_landing_review_pool().source.label().to_string(),
        crew_pool,
        budget: policy.review_budget(),
        required_validation_commands: Some(
            runtime.workflow_required_validation_commands().to_vec(),
        ),
        baseline_commands: runtime.review_baseline_commands().to_vec(),
        host_evidence: policy.review_host_evidence.value.clone(),
        captured_at: Utc::now(),
    })
}

/// The review admission a running pipeline was admitted under, read from its
/// persisted run input.
pub(crate) fn run_review_admission(
    runtime: &OrbitRuntime,
    run_id: &str,
) -> Result<Option<ReviewAdmission>, OrbitError> {
    let Some(run) = runtime.get_job_run_backend(run_id)? else {
        return Ok(None);
    };
    run.input
        .as_ref()
        .map(ReviewAdmission::from_run_input)
        .transpose()
        .map_err(OrbitError::from)
        .map(Option::flatten)
}

/// The review layer an admission captured and what decided it, as run
/// surfaces show it [ORB-16572]: `source` is `run-option` when the run's
/// `--review` added it, else the policy layer (`workspace`, `global`,
/// `built-in`) or `claim` for a claimed leaf.
pub(crate) fn review_layer_json(admission: &ReviewAdmission) -> Value {
    serde_json::json!({
        "timing": admission.timing.as_str(),
        "source": admission.timing_source,
    })
}

impl OrbitRuntime {
    /// A run's captured review layer as `{"timing", "source"}`, or null
    /// when it captured no review admission.
    pub fn run_review_layer(&self, run_id: &str) -> Result<Value, OrbitError> {
        Ok(run_review_admission(self, run_id)?
            .as_ref()
            .map_or(Value::Null, review_layer_json))
    }
}

/// The review admission of the run an activity executes in, named by the
/// engine-injected `run_id` of its input.
pub(crate) fn input_run_review_admission(
    runtime: &OrbitRuntime,
    input: &Value,
) -> Result<Option<ReviewAdmission>, OrbitError> {
    match input
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|run_id| !run_id.is_empty())
    {
        Some(run_id) => run_review_admission(runtime, run_id),
        None => Ok(None),
    }
}

/// Explain why an automatically resumed delivery run cannot safely keep its
/// captured review admission. A resume reuses successful checkpoints, so
/// installing today's admission into its input would not rerun an already
/// successful review gate. The clock therefore resumes only when the captured
/// admission still matches today's contract.
pub(crate) fn upgrade_resume_admission_mismatch(
    runtime: &OrbitRuntime,
    run: &orbit_types::workflow::JobRun,
) -> Option<String> {
    if !super::REVIEW_ADMITTED_JOBS.contains(&run.job_id.as_str()) {
        return None;
    }
    let Some(input) = run.input.as_ref() else {
        return Some("run has no captured review admission".to_string());
    };
    let previous = match ReviewAdmission::from_run_input(input) {
        Ok(Some(admission)) => admission,
        Ok(None) => return Some("run has no captured review admission".to_string()),
        Err(error) => return Some(error.to_string()),
    };
    // A run that captured its layer from `--review` resolves today's policy
    // with the same request, so the option alone is never a mismatch.
    let request = match ReviewLayer::from_run_input(input) {
        Ok(request) => request.or_else(|| {
            (previous.timing_source == REVIEW_RUN_OPTION_SOURCE)
                .then(|| ReviewLayer::from_timing(previous.timing))
                .flatten()
        }),
        Err(error) => return Some(error.to_string()),
    };
    let current = match resolve(runtime, request) {
        Ok(current) => current,
        Err(error) => return Some(error.to_string()),
    };
    let mut comparable_previous = previous;
    // Capture time is provenance, not part of the admission contract.
    comparable_previous.captured_at = current.captured_at;
    (comparable_previous != current).then(|| {
        "the captured review admission differs from the current review policy; submit a new run to capture it"
            .to_string()
    })
}

fn reserved_review_key_error(job_name: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "run input for job '{job_name}' set the reserved `{REVIEW_ADMISSION_KEY}` field; the \
         effective review.timing is captured from configuration at submission and cannot be \
         requested through ordinary job input"
    ))
}
