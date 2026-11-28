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
use orbit_types::task::{
    ArtifactManifestFileV2, SECURITY_STATE_REVIEW_TAG, Task, requires_security_state_review,
};
use orbit_types::workflow::{
    self, REVIEW_ADMISSION_KEY, REVIEW_CONTRACT_VERSION, REVIEW_REQUEST_KEY,
    REVIEW_RUN_OPTION_SOURCE, ReviewAdmission, ReviewLayer, ReviewTiming,
};
use serde_json::Value;

use super::REVIEW_ADMITTED_JOBS;
use super::crew::pre_landing_review_crew_unset;
use crate::OrbitRuntime;
use crate::application::contracts::review_layer_json_for_run;
use crate::application::job::ReviewAdmissionHook;
use crate::application::task::TaskReviewPort;
use crate::runtime::run_input::carried_task_ids;

impl OrbitRuntime {
    /// A run's captured review layer as `{"timing", "source"}` (plus
    /// `"scope": "supervisor_local"` on pull supervisors), or null when it
    /// captured no review admission.
    pub fn run_review_layer(&self, run_id: &str) -> Result<Value, OrbitError> {
        let Some(run) = self.get_job_run_backend(run_id)? else {
            return Ok(Value::Null);
        };
        let admission = run
            .input
            .as_ref()
            .map(ReviewAdmission::from_run_input)
            .transpose()
            .map_err(OrbitError::from)?
            .flatten();
        Ok(admission.as_ref().map_or(Value::Null, |admission| {
            review_layer_json_for_run(&run.job_id, admission)
        }))
    }

    /// Authenticate the exact-head review for a security-state delivery.
    pub(crate) fn security_state_review_ready(
        &self,
        task: &Task,
        head: &str,
    ) -> Result<bool, OrbitError> {
        self.handoff_landing_review_ready(task, Some(head), None)
    }

    /// Read the task tags from their authoritative records.
    pub(super) fn delivery_requires_security_state_review<'a>(
        &self,
        task_ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<bool, OrbitError> {
        for id in task_ids {
            if requires_security_state_review(&self.get_task(id)?.tags) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl OrbitRuntime {
    pub(super) fn handoff_landing_review_ready_with(
        &self,
        task: &Task,
        head: Option<&str>,
        manifest: Option<&[ArtifactManifestFileV2]>,
        observational: bool,
    ) -> Result<bool, OrbitError> {
        use crate::application::job::ReviewAdmissionHook;
        use orbit_types::task::ArtifactWriter;
        use orbit_types::workflow::{REVIEW_GATE_ARTIFACT, ReviewCertificate, ReviewTiming};
        // No handoff head means no candidate to cover, whatever the review policy.
        let Some(head) = head else {
            return Ok(false);
        };
        let captured = task
            .job_run_id
            .as_deref()
            .map(|id| self.captured_review_admission(id))
            .transpose()?
            .flatten();
        let gated = |timing| matches!(timing, ReviewTiming::BeforePr | ReviewTiming::BeforeLanding);
        let policy = self.operation_policy();
        let mut security_review = requires_security_state_review(&task.tags);
        if !security_review && let Some(run_id) = &task.job_run_id {
            let run_tasks = if observational {
                self.observe_run_tasks(run_id)?
            } else {
                self.list_run_tasks(run_id)?
            };
            security_review = run_tasks
                .iter()
                .any(|member| requires_security_state_review(&member.tags));
        }
        if !security_review
            && !policy.review_before_pr.value
            && !policy.review_before_landing.value
            && !captured.is_some_and(|admission| gated(admission.timing))
        {
            return Ok(true);
        }
        let use_snapshot = manifest.is_some() && !self.stored().task().held_by_owner();
        let loaded_manifest;
        let manifest = match manifest {
            Some(manifest) => manifest,
            None => {
                loaded_manifest = self.get_task_artifact_manifest(&task.id)?;
                &loaded_manifest
            }
        };
        if !manifest.iter().any(|file| {
            file.path == REVIEW_GATE_ARTIFACT && file.writer == Some(ArtifactWriter::System)
        }) {
            return Ok(false);
        }
        let Some(file) = manifest
            .iter()
            .find(|file| file.path == REVIEW_GATE_ARTIFACT)
        else {
            return Ok(false);
        };
        let artifact = if observational {
            self.observe_task_artifact(&task.id, REVIEW_GATE_ARTIFACT)?
        } else if use_snapshot {
            Some(
                self.stores()
                    .task_artifacts()
                    .read_manifest_artifact(&task.id, file)?,
            )
        } else {
            self.get_task_artifact(&task.id, REVIEW_GATE_ARTIFACT)?
        };
        let Some(artifact) = artifact else {
            return Ok(false);
        };
        let Ok(certificate) = serde_json::from_slice::<ReviewCertificate>(&artifact.content) else {
            return Ok(false);
        };
        if !certificate.verdict.passed()
            || !certificate.validation_complete
            || !certificate.task_ids.contains(&task.id)
            || (security_review
                && orbit_automation::review::certificate_acceptable(&certificate).is_err())
        {
            return Ok(false);
        }
        if certificate.final_candidate.commit != head {
            if security_review {
                return Ok(false);
            }
            let base_branch = self.delivery_target_branch(task);
            let pr_number = task
                .external_refs
                .iter()
                .rev()
                .filter(|reference| {
                    reference.system == orbit_types::task::GITHUB_PR_EXTERNAL_REF_SYSTEM
                })
                .find_map(|reference| reference.id.parse::<u64>().ok());
            if orbit_engine::verify_refreshed_head_from_base(
                self.env(),
                &self.paths().repo_root,
                &certificate.final_candidate.commit,
                head,
                &base_branch,
                None,
                pr_number,
            )
            .is_err()
            {
                return Ok(false);
            }
        }
        let meanings = certificate
            .task_ids
            .iter()
            .map(|id| {
                let loaded_task;
                let covered_task = if id == &task.id {
                    task
                } else {
                    loaded_task = if observational {
                        self.observe_task(id)?
                    } else {
                        self.get_task(id)?
                    };
                    &loaded_task
                };
                let digest = orbit_automation::review::task_meaning_digest(covered_task)
                    .map_err(super::automation_error)?;
                Ok((id.clone(), digest))
            })
            .collect::<Result<Vec<_>, OrbitError>>()?;
        if orbit_automation::review::combined_task_meaning_digest(&meanings)
            .map_err(super::automation_error)?
            != certificate.task_meaning_digest
        {
            return Ok(false);
        }
        // The immutable owner ledger authenticates the artifact's certificate.
        Ok(self
            .sqlite_store()?
            .read()
            .and_then(|snapshot| {
                snapshot
                    .review()
                    .certificate(&self.workspace_id()?, &certificate.attempt_id)
            })?
            .as_ref()
            == Some(&certificate))
    }
}

impl ReviewAdmissionHook for OrbitRuntime {
    fn install_review_admission(
        &self,
        job_name: &str,
        input: &mut Value,
        parent_run_id: Option<&str>,
        resuming: bool,
    ) -> Result<(), OrbitError> {
        install_review_admission(self, job_name, input, parent_run_id, resuming)
    }

    fn admit_review_request(
        &self,
        input: &mut Value,
        review: Option<ReviewLayer>,
        local_route: bool,
    ) -> Result<(), OrbitError> {
        admit_review_request(self, input, review, local_route)
    }

    fn local_route_review_refusal(&self, layer: ReviewLayer) -> String {
        local_route_run_option_refusal(layer)
    }

    fn captured_review_admission(
        &self,
        run_id: &str,
    ) -> Result<Option<ReviewAdmission>, OrbitError> {
        run_review_admission(self, run_id)
    }

    fn release_run_review_attempts(
        &self,
        run_id: &str,
        finished_at: chrono::DateTime<chrono::Utc>,
    ) {
        self.best_effort_release_run_review_attempts(run_id, finished_at);
    }

    fn upgrade_resume_review_mismatch(
        &self,
        run: &orbit_types::workflow::JobRun,
    ) -> Option<String> {
        upgrade_resume_admission_mismatch(self, run)
    }

    fn completion_review_mismatch(&self, run: &orbit_types::workflow::JobRun) -> Option<String> {
        completion_admission_mismatch(self, run)
    }
}

/// Whether caller-shaped run input names the reserved review key.
fn declares_review_admission(input: &Value) -> bool {
    input
        .get(REVIEW_ADMISSION_KEY)
        .is_some_and(|value| !value.is_null())
}

/// Install the review admission a delivery submission carries, or refuse a
/// submission that tries to supply one. `parent_run_id` is set for the
/// parent-authorized child path; `resuming` keeps persisted input as is.
fn install_review_admission(
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
    if request.is_some() && job_name == crate::application::contracts::PULL_DRAIN_JOB {
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
    // included. A PR leaf applies its task obligations to that snapshot.
    let mut admission = match inherited {
        Some(admission) => admission,
        None => resolve(runtime, request)?,
    };
    if job_name == workflow::TASK_PR_PIPELINE_JOB
        && runtime.delivery_requires_security_state_review(
            carried_task_ids(input).iter().map(String::as_str),
        )?
    {
        apply_security_state_review(&mut admission);
    }
    if job_name == workflow::TASK_LOCAL_PIPELINE_JOB
        && let Some(layer) = ReviewLayer::from_timing(admission.timing)
    {
        if runtime.delivery_requires_security_state_review(
            carried_task_ids(input).iter().map(String::as_str),
        )? {
            return Err(OrbitError::InvalidInput(format!(
                "`{SECURITY_STATE_REVIEW_TAG}` requires review before landing; ship through the PR route"
            )));
        }
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

/// Retain the configured layer, adding before-landing review when it is off.
pub(super) fn apply_security_state_review(admission: &mut ReviewAdmission) {
    if !admission.gates_merge() {
        admission.timing = ReviewTiming::BeforeLanding;
    }
    admission.timing_source = SECURITY_STATE_REVIEW_TAG.to_string();
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
fn admit_review_request(
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
        warn_before_pr_policy(&admission);
        return Ok(admission);
    };
    if admission.timing == layer.timing() {
        require_reviewer_crew(&admission, layer, Some(&admission.timing_source))?;
        warn_before_pr_request(&admission, layer, true);
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
    warn_before_pr_request(&admission, layer, false);
    Ok(admission)
}

/// Bounded deprecation warning for a before-PR admission [ORB-20099].
///
/// Emitted once per fresh submission through `tracing` (stderr/log), never in
/// stdout JSON or MCP envelopes. Effective behavior stays before-PR; the
/// warning names the deciding key/request and its source so an operator can
/// migrate with an explicit config write or a `--review before-landing`
/// request.
fn warn_before_pr_policy(admission: &ReviewAdmission) {
    if admission.timing != ReviewTiming::BeforePr {
        return;
    }
    tracing::warn!(
        key = "review.timing",
        value = "before_pr",
        source = %admission.timing_source,
        note = "review.timing = before_pr is deprecated but still holds PR creation for a reviewer; new setups use off or before_landing (`orbit config set review.timing before_landing`)",
        "review timing is deprecated but still honoured as before-PR",
    );
}

/// Bounded deprecation warning for an explicit old `--review before-pr`
/// request [ORB-20099]. `policy_match` when the workspace already applies
/// before-PR and the request only restates it.
fn warn_before_pr_request(admission: &ReviewAdmission, layer: ReviewLayer, policy_match: bool) {
    if !layer.is_deprecated() || admission.timing != ReviewTiming::BeforePr {
        return;
    }
    if policy_match {
        tracing::warn!(
            request = "--review before-pr",
            source = %admission.timing_source,
            note = "--review before-pr is deprecated but still holds PR creation for a reviewer; new requests use --review before-landing",
            "review request is deprecated but still honoured as before-PR",
        );
    } else {
        tracing::warn!(
            request = "--review before-pr",
            source = %REVIEW_RUN_OPTION_SOURCE,
            note = "--review before-pr is deprecated but still holds PR creation for a reviewer; new requests use --review before-landing",
            "review request is deprecated but still honoured as before-PR",
        );
    }
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
        selective_before_landing: None,
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
fn upgrade_resume_admission_mismatch(
    runtime: &OrbitRuntime,
    run: &orbit_types::workflow::JobRun,
) -> Option<String> {
    if !super::REVIEW_ADMITTED_JOBS.contains(&run.job_id.as_str()) {
        return None;
    }
    let (mut previous, current) = match resume_admissions(runtime, run) {
        Ok(pair) => pair,
        Err(reason) => return Some(reason),
    };
    previous.captured_at = current.captured_at;
    ignore_legacy_selective_policy(&mut previous);
    (previous != current).then(|| {
        "the captured review admission differs from the current review policy; submit a new run to capture it"
            .to_string()
    })
}

/// [ORB-22808] A selective policy an earlier build captured is ignored, so
/// it is no drift: nothing evaluates it and no current admission carries one.
fn ignore_legacy_selective_policy(previous: &mut ReviewAdmission) {
    previous.selective_before_landing = None;
}

/// Unlike ordinary checkpoint resume, completion observation only needs the
/// obligations that apply to this unchanged candidate. Provenance-only drift
/// and the retired after-landing state cannot add a pre-landing gate.
fn completion_admission_mismatch(
    runtime: &OrbitRuntime,
    run: &orbit_types::workflow::JobRun,
) -> Option<String> {
    let (mut previous, current) = match resume_admissions(runtime, run) {
        Ok(pair) => pair,
        Err(reason) => return Some(reason),
    };
    ignore_legacy_selective_policy(&mut previous);
    let gated = |timing| matches!(timing, ReviewTiming::BeforePr | ReviewTiming::BeforeLanding);
    let same = if !gated(previous.timing) && !gated(current.timing) {
        previous.contract_version == current.contract_version
            && previous.policy_version == current.policy_version
            && previous.required_validation_commands == current.required_validation_commands
    } else {
        previous.captured_at = current.captured_at;
        previous.timing_source.clone_from(&current.timing_source);
        previous.crew_source.clone_from(&current.crew_source);
        previous == current
    };
    if same {
        return None;
    }
    let changed = [
        (previous.timing != current.timing, "timing"),
        (
            previous.contract_version != current.contract_version
                || previous.policy_version != current.policy_version,
            "contract version",
        ),
        (
            previous.crew != current.crew || previous.crew_pool != current.crew_pool,
            "reviewer crew/pool",
        ),
        (previous.budget != current.budget, "review budget"),
        (
            previous.required_validation_commands != current.required_validation_commands,
            "required validation commands",
        ),
        (
            previous.baseline_commands != current.baseline_commands,
            "baseline commands",
        ),
        (
            previous.host_evidence != current.host_evidence,
            "host evidence",
        ),
    ]
    .into_iter()
    .filter_map(|(different, name)| different.then_some(name))
    .collect::<Vec<_>>();
    Some(format!(
        "candidate review obligations changed: {} (captured {}, current {}); require current-policy candidate recovery",
        changed.join(", "),
        previous.timing.as_str(),
        current.timing.as_str(),
    ))
}

fn resume_admissions(
    runtime: &OrbitRuntime,
    run: &orbit_types::workflow::JobRun,
) -> Result<(ReviewAdmission, ReviewAdmission), String> {
    let input = run
        .input
        .as_ref()
        .ok_or("run has no captured review admission")?;
    let previous = ReviewAdmission::from_run_input(input)
        .map_err(|error| error.to_string())?
        .ok_or("run has no captured review admission")?;
    // A run that captured its layer from `--review` resolves today's policy
    // with the same request, so the option alone is never a mismatch.
    let request = match ReviewLayer::from_run_input(input) {
        Ok(request) => request.or_else(|| {
            (previous.timing_source == REVIEW_RUN_OPTION_SOURCE)
                .then(|| ReviewLayer::from_timing(previous.timing))
                .flatten()
        }),
        Err(error) => return Err(error.to_string()),
    };
    let mut current = resolve(runtime, request).map_err(|error| error.to_string())?;
    if run.job_id == workflow::TASK_PR_PIPELINE_JOB
        && runtime
            .delivery_requires_security_state_review(
                carried_task_ids(input).iter().map(String::as_str),
            )
            .map_err(|error| error.to_string())?
    {
        apply_security_state_review(&mut current);
    }
    Ok((previous, current))
}

fn reserved_review_key_error(job_name: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "run input for job '{job_name}' set the reserved `{REVIEW_ADMISSION_KEY}` field; the \
         effective review.timing is captured from configuration at submission and cannot be \
         requested through ordinary job input"
    ))
}
