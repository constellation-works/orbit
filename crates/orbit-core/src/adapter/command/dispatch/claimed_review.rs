//! The claimed before-PR reviewer's artifacts through its run's broker.
//!
//! A claimed leaf's reviewer reads its pinned manifest and prior review
//! evidence, and attaches its report, on the owner's task through the
//! claimed-owner bridge ([`super::claimed_owner`]), which carries every
//! claimed worker's owner calls. The before-PR gate's own artifacts are
//! narrower than the rest: the broker answers them only for the before-PR
//! reviewer, and only for the one admitted attempt whose reviewer is running
//! in this run, as the review ledger records it. A read is carried for the
//! review contract's own artifacts and for the evidence artifacts the owner's
//! current evidence hold names, resolved from the owner's hold, never from the
//! request. A write is carried only for the report. The manifest the owner
//! returns and the report the reviewer wrote must both be that attempt's; the
//! report's bytes are validated before the owner's claim transaction persists
//! them.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::tool::{ToolSessionContext, WorkerInvocation};
use orbit_types::workflow::{
    REVIEW_CONTRACT_VERSION, REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_GATE_ARTIFACT,
    REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT, REVIEW_REPORT_HISTORY_ARTIFACT,
    ReviewAttemptState, ReviewEvidenceHold, ReviewExternalEvidence, ReviewManifest, ReviewReport,
};
use serde_json::{Map, Value, json};

use super::claimed_owner::{
    GET, PUT, accept_fields, artifact_put_input, put_content, require_claimed_task,
};
use crate::OrbitRuntime;

/// The review contract's artifacts the reviewer may always read: its pinned
/// manifest and the prior evidence it continues from. Any other read must be
/// named by the owner's current evidence hold.
const CONTRACT_READS: [&str; 4] = [
    REVIEW_MANIFEST_ARTIFACT,
    REVIEW_REPORT_ARTIFACT,
    REVIEW_REPORT_HISTORY_ARTIFACT,
    REVIEW_EVIDENCE_HOLD_ARTIFACT,
];

/// Whether the canonical artifact `path` is in the before-PR gate's artifact
/// namespace, which only the running reviewer's attempt scope reaches through
/// the broker. The gate names every artifact it writes or reads `review-*`;
/// refusing the whole prefix keeps a worker from forging one the gate adds
/// later. The prefix is matched without ASCII case, so an owner on a
/// case-insensitive filesystem cannot be handed a `Review-gate.json` whose
/// blob is the certificate's.
pub(super) fn is_review_artifact(path: &str) -> bool {
    path.get(..REVIEW_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(REVIEW_PREFIX))
}

const REVIEW_PREFIX: &str = "review-";

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
/// the broker, for the claim [`super::claimed_owner`] derived. `path` is the
/// request's canonical artifact path; every check and the owner's call use
/// it, never the raw string. A hold-named evidence artifact outside the
/// `review-*` namespace is any claimed worker's ordinary artifact read, so it
/// never reaches this scope.
pub(super) fn execute_brokered(
    runtime: &OrbitRuntime,
    run: &orbit_engine::PluginBrokerRun,
    binding: &WorkerInvocation,
    tool: &str,
    object: &Map<String, Value>,
    path: &str,
    session: ToolSessionContext,
) -> Result<Value, OrbitError> {
    let scope = ClaimedReviewScope::derive(runtime, run, binding)?;
    if tool == PUT {
        accept_fields(object, &["id", "path", "content_base64", "model"])?;
    } else {
        accept_fields(object, &["id", "path", "model"])?;
    }
    require_claimed_task(object, binding)?;
    if tool == PUT && path != REVIEW_REPORT_ARTIFACT {
        return Err(denied(&format!(
            "'{PUT}' carries only `{REVIEW_REPORT_ARTIFACT}` for the claimed reviewer"
        )));
    }
    if tool == GET && !CONTRACT_READS.contains(&path) && !evidence_path(path) {
        return Err(read_refused());
    }
    runtime.authorize_tool_operation(
        tool,
        &session,
        crate::runtime::tool_exec::CapabilityEnforcement::McpSessionOnly,
    )?;
    if tool == GET {
        if !CONTRACT_READS.contains(&path) && !scope.hold_names(runtime, path, &session)? {
            return Err(read_refused());
        }
        let output = scope.read(runtime, path, object.get("model"), session)?;
        if path == REVIEW_MANIFEST_ARTIFACT {
            scope.check_manifest(&output)?;
        }
        return Ok(output);
    }
    let content = put_content(object)?;
    scope.check_report(&content)?;
    runtime.route_worker_tool(
        PUT,
        artifact_put_input(
            binding,
            REVIEW_REPORT_ARTIFACT,
            content,
            object.get("model"),
        ),
        session,
    )
}

/// Whether `path` could be an evidence artifact a hold names: a valid
/// relative artifact path that is not one of the review contract's own.
fn evidence_path(path: &str) -> bool {
    orbit_types::task::validate_relative_artifact_path(path).is_ok()
        && !CONTRACT_READS.contains(&path)
        && path != REVIEW_GATE_ARTIFACT
}

fn read_refused() -> OrbitError {
    denied(&format!(
        "'{GET}' carries only `{REVIEW_MANIFEST_ARTIFACT}`, `{REVIEW_REPORT_ARTIFACT}`, \
         `{REVIEW_REPORT_HISTORY_ARTIFACT}`, `{REVIEW_EVIDENCE_HOLD_ARTIFACT}` and the evidence \
         artifacts the owner's current evidence hold names for the claimed reviewer"
    ))
}

/// Whether an owner error says the artifact does not exist, in process or
/// across the owner route.
fn not_found(error: &OrbitError) -> bool {
    match error {
        OrbitError::NotFound { .. } => true,
        OrbitError::RemoteTool { code, .. } => code == "not_found",
        _ => false,
    }
}

/// The artifact bytes in an owner's `orbit.task.artifact.get` answer.
fn artifact_bytes(output: &Value, path: &str) -> Result<Vec<u8>, OrbitError> {
    match (output.get("content"), output.get("content_base64")) {
        (Some(Value::String(text)), _) => Ok(text.as_bytes().to_vec()),
        (_, Some(Value::String(encoded))) => BASE64_STANDARD
            .decode(encoded)
            .map_err(|error| OrbitError::Execution(format!("owner {path} payload: {error}"))),
        _ => Err(OrbitError::Execution(format!(
            "the owner answered the {path} read without its bytes"
        ))),
    }
}

impl ClaimedReviewScope<'_> {
    /// Read the claimed task's artifact at `path` from the owner, whose claim
    /// fence answers only while the claim is active.
    fn read(
        &self,
        runtime: &OrbitRuntime,
        path: &str,
        model: Option<&Value>,
        session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let mut input = json!({"id": self.task_id(), "path": path});
        if let Some(model) = model {
            input["model"] = model.clone();
        }
        runtime.route_worker_tool(GET, input, session)
    }

    /// The bytes of the owner's artifact at `path`, or `None` when it has none.
    fn owner_bytes(
        &self,
        runtime: &OrbitRuntime,
        path: &str,
        session: &ToolSessionContext,
    ) -> Result<Option<Vec<u8>>, OrbitError> {
        match self.read(runtime, path, None, session.clone()) {
            Ok(output) => artifact_bytes(&output, path).map(Some),
            Err(error) if not_found(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Whether the owner's current evidence hold names `path`: as a
    /// requirement's result artifact, or as the log that result names for
    /// that requirement. The names come from the owner's records only; an
    /// absent or unreadable hold names nothing.
    fn hold_names(
        &self,
        runtime: &OrbitRuntime,
        path: &str,
        session: &ToolSessionContext,
    ) -> Result<bool, OrbitError> {
        let Some(bytes) = self.owner_bytes(runtime, REVIEW_EVIDENCE_HOLD_ARTIFACT, session)? else {
            return Ok(false);
        };
        let Ok(hold) = serde_json::from_slice::<ReviewEvidenceHold>(&bytes) else {
            return Ok(false);
        };
        if hold
            .requirements
            .iter()
            .any(|required| required.artifact == path)
        {
            return Ok(true);
        }
        for required in &hold.requirements {
            if !evidence_path(&required.artifact) {
                continue;
            }
            let Some(bytes) = self.owner_bytes(runtime, &required.artifact, session)? else {
                continue;
            };
            let Ok(evidence) = serde_json::from_slice::<ReviewExternalEvidence>(&bytes) else {
                continue;
            };
            if evidence.log_artifact == path
                && evidence.matches_requirement(required, &hold.candidate)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The manifest the owner returned must be the admitted attempt's: the
    /// gate writes it once per attempt, so another attempt's manifest is stale.
    fn check_manifest(&self, output: &Value) -> Result<(), OrbitError> {
        let bytes = artifact_bytes(output, REVIEW_MANIFEST_ARTIFACT)?;
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
