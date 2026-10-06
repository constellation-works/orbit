//! The claimed before-PR reviewer's artifacts through its run's broker.
//!
//! A claimed leaf's reviewer reads its pinned manifest and attaches its report
//! on the owner's task through the claimed-owner bridge
//! ([`super::claimed_owner`]), which carries every claimed worker's owner
//! calls. The before-PR gate's own artifacts are narrower than the rest: the
//! broker answers them only for the before-PR reviewer, only the manifest read
//! and the report write, and only for the one admitted attempt whose reviewer
//! is running in this run, as the review ledger records it. The manifest the
//! owner returns and the report the reviewer wrote must both be that
//! attempt's; the report's bytes are validated before the owner's claim
//! transaction persists them.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::tool::{ToolSessionContext, WorkerInvocation};
use orbit_types::workflow::{
    REVIEW_CONTRACT_VERSION, REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT, ReviewAttemptState,
    ReviewManifest, ReviewReport,
};
use serde_json::{Map, Value, json};

use super::claimed_owner::{
    GET, PUT, accept_fields, artifact_put_input, put_content, require_claimed_task,
};
use crate::OrbitRuntime;

/// Whether `path` is in the before-PR gate's artifact namespace, which only
/// the running reviewer's attempt scope reaches through the broker. The gate
/// names every artifact it writes or reads `review-*`; refusing the whole
/// prefix keeps a worker from forging one the gate adds later.
pub(super) fn is_review_artifact(path: &str) -> bool {
    path.starts_with("review-")
}

/// The review attempt a broker serves, derived from host records.
struct ClaimedReviewScope<'a> {
    binding: &'a WorkerInvocation,
    attempt_id: String,
    lineage_key: String,
}

impl<'a> ClaimedReviewScope<'a> {
    /// Refuse unless this run, already checked as the claimed leaf of
    /// `binding`, is its reviewer with exactly one admitted attempt whose
    /// reviewer is running in it now.
    fn derive(
        runtime: &OrbitRuntime,
        run: &orbit_engine::PluginBrokerRun,
        binding: &'a WorkerInvocation,
    ) -> Result<Self, OrbitError> {
        if run.activity_name != orbit_engine::review_gate::REVIEWER_ACTIVITY {
            return Err(denied(&format!(
                "the broker carries review artifacts only for the before-PR reviewer, not \
                 activity '{}'",
                run.activity_name
            )));
        }
        let job_run_id = run
            .job_run_id
            .as_deref()
            .ok_or_else(|| denied("the run has no job-run authority"))?;
        let now = Utc::now();
        let mut held = Vec::new();
        for ledger in runtime
            .review_store()?
            .review_ledgers_held_by(&runtime.workspace_id()?, job_run_id)?
        {
            if !ledger.task_ids.contains(&binding.task_id) {
                continue;
            }
            for attempt in &ledger.attempts {
                let running = attempt
                    .reviewer_running
                    .as_ref()
                    .is_some_and(|running| running.run_id == job_run_id && now < running.deadline);
                if running
                    && attempt.state == ReviewAttemptState::Open
                    && attempt.run_id == binding.bound_run_id
                {
                    held.push((attempt.attempt_id.clone(), ledger.lineage_key.clone()));
                }
            }
        }
        match held.as_slice() {
            [(attempt_id, lineage_key)] => Ok(Self {
                binding,
                attempt_id: attempt_id.clone(),
                lineage_key: lineage_key.clone(),
            }),
            [] => Err(denied(
                "review_attempt_stale: no admitted review attempt has its reviewer running in \
                 this run; the attempt was settled, released or timed out",
            )),
            _ => Err(denied(
                "more than one open review attempt names this run; refusing to choose",
            )),
        }
    }

    fn task_id(&self) -> &str {
        &self.binding.task_id
    }
}

fn denied(reason: &str) -> OrbitError {
    OrbitError::PolicyDenied(format!("claimed_review_bridge_refused: {reason}"))
}

/// Execute one bridged call on a [review artifact](is_review_artifact) in
/// the broker, for the claim [`super::claimed_owner`] derived.
pub(super) fn execute_brokered(
    runtime: &OrbitRuntime,
    run: &orbit_engine::PluginBrokerRun,
    binding: &WorkerInvocation,
    tool: &str,
    object: &Map<String, Value>,
    session: ToolSessionContext,
) -> Result<Value, OrbitError> {
    let scope = ClaimedReviewScope::derive(runtime, run, binding)?;
    if tool == PUT {
        accept_fields(object, &["id", "path", "content_base64", "model"])?;
    } else {
        accept_fields(object, &["id", "path", "model"])?;
    }
    require_claimed_task(object, binding)?;
    let expected = if tool == PUT {
        REVIEW_REPORT_ARTIFACT
    } else {
        REVIEW_MANIFEST_ARTIFACT
    };
    if object.get("path").and_then(Value::as_str) != Some(expected) {
        return Err(denied(&format!(
            "'{tool}' carries only `{expected}` for the claimed reviewer"
        )));
    }
    runtime.authorize_tool_operation(
        tool,
        &session,
        crate::runtime::tool_exec::CapabilityEnforcement::McpSessionOnly,
    )?;
    if tool == GET {
        let mut owner_input = json!({"id": scope.task_id(), "path": expected});
        if let Some(model) = object.get("model") {
            owner_input["model"] = model.clone();
        }
        let output = runtime.route_worker_tool(GET, owner_input, session)?;
        scope.check_manifest(&output)?;
        return Ok(output);
    }
    let content = put_content(object)?;
    scope.check_report(&content)?;
    runtime.route_worker_tool(
        PUT,
        artifact_put_input(binding, expected, content, object.get("model")),
        session,
    )
}

impl ClaimedReviewScope<'_> {
    /// The manifest the owner returned must be the admitted attempt's: the
    /// gate writes it once per attempt, so another attempt's manifest is stale.
    fn check_manifest(&self, output: &Value) -> Result<(), OrbitError> {
        let bytes = match (output.get("content"), output.get("content_base64")) {
            (Some(Value::String(text)), _) => text.as_bytes().to_vec(),
            (_, Some(Value::String(encoded))) => {
                BASE64_STANDARD.decode(encoded).map_err(|error| {
                    OrbitError::Execution(format!("owner manifest payload: {error}"))
                })?
            }
            _ => {
                return Err(OrbitError::Execution(
                    "the owner answered the manifest read without its bytes".into(),
                ));
            }
        };
        let manifest: ReviewManifest = serde_json::from_slice(&bytes).map_err(|error| {
            OrbitError::Execution(format!("{REVIEW_MANIFEST_ARTIFACT} is unreadable: {error}"))
        })?;
        if manifest.attempt_id != self.attempt_id
            || manifest.lineage_key != self.lineage_key
            || !manifest.task_ids.iter().any(|id| id == self.task_id())
        {
            return Err(OrbitError::PolicyDenied(format!(
                "claimed_review_bridge_refused: review_manifest_stale: the owner's manifest is \
                 for attempt '{}', not the running attempt '{}'",
                manifest.attempt_id, self.attempt_id
            )));
        }
        Ok(())
    }

    /// A report must be the review contract's, for the running attempt. The
    /// owner's attach validates the contract again; checking here keeps a
    /// report for another attempt from reaching the owner at all.
    fn check_report(&self, content: &[u8]) -> Result<(), OrbitError> {
        let report = ReviewReport::parse(content).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "{REVIEW_REPORT_ARTIFACT} does not match the review report contract: {error}"
            ))
        })?;
        if report.schema_version != REVIEW_CONTRACT_VERSION {
            return Err(OrbitError::InvalidInput(format!(
                "{REVIEW_REPORT_ARTIFACT} has schema_version {}; the review report contract is \
                 version {REVIEW_CONTRACT_VERSION}",
                report.schema_version
            )));
        }
        if report.attempt_id != self.attempt_id {
            return Err(OrbitError::PolicyDenied(format!(
                "claimed_review_bridge_refused: the report names attempt '{}', not the running \
                 attempt '{}'",
                report.attempt_id, self.attempt_id
            )));
        }
        Ok(())
    }
}
