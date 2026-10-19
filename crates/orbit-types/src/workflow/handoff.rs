//! Exact delivery evidence and owner completion authority for distributed handoffs.
//! These records execute no merge; a before-PR review they carry is evidence
//! the owner verifies, never authority on its own.
use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{ReviewTiming, ReviewVerdict, automation::SourceRevision};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffArtifactRef {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HandoffDelivery {
    PullRequest {
        number: u64,
    },
    LocalCandidate,
    /// A clean base with a digest-pinned clean-tree verifier checkpoint.
    /// Covers both a no-op implementation and work already satisfied on base.
    NoDiff {
        evidence: HandoffArtifactRef,
    },
    AlreadyLanded {
        covering_commit: String,
        evidence: HandoffArtifactRef,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffCandidate {
    pub repository: String,
    pub source_branch: String,
    pub base_branch: String,
    pub landing_branch: String,
    pub candidate: SourceRevision,
    pub base: SourceRevision,
    pub delivery: HandoffDelivery,
}

/// What the leaf's review settled. `not_required` keeps its original
/// spelling, so handoffs recorded before before-PR evidence existed still
/// read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffReviewDisposition {
    /// The claim's contract captured no review before landing: no reviewed
    /// SHA, verdict or reviewer artifact exists, and none is invented.
    NotRequired,
    /// The leaf ran the before-PR gate the claim's contract captured
    /// [ORB-13895].
    BeforePr(Box<HandoffReviewEvidence>),
    /// The leaf reviewed its open pull request before handing it off, as
    /// the claim's contract captured `review.timing = before_landing` [ORB-14849].
    BeforeLanding(Box<HandoffReviewEvidence>),
}

/// The leaf's review of the candidate it hands off. Every field is
/// a claim the owner checks against the certificate artifact it holds and
/// its own observation of the candidate; none is trusted on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffReviewEvidence {
    /// The review attempt the certificate was issued for.
    pub attempt_id: String,
    pub verdict: ReviewVerdict,
    /// The head the verdict binds to: the candidate after the reviewer's fix
    /// commit when it made one. It must be the handed-off candidate.
    pub reviewed_head_sha: String,
    /// The base the reviewer examined the candidate against.
    pub reviewed_base_sha: String,
    /// The reviewer's own fix commit (the candidate's last commit), when it
    /// fixed findings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer_commit: Option<String>,
    pub reviewer_crew: String,
    /// The run the reviewer was invoked from.
    pub reviewer_run_id: String,
    /// The settled [`ReviewCertificate`](super::ReviewCertificate) in an owner-accessible task artifact.
    pub certificate: HandoffArtifactRef,
    /// Further reviewer evidence (manifest, report) the owner holds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<HandoffArtifactRef>,
    /// [ORB-14478] The result and log of every `host_sandbox_test` the
    /// leaf's host ran for this verdict, which the owner re-reads and checks
    /// against the certificate and candidate tree.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_evidence: Vec<HandoffArtifactRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffReview {
    pub policy: ReviewTiming,
    pub disposition: HandoffReviewDisposition,
}

impl HandoffReview {
    /// The disposition of a claim admitted with no review before landing.
    pub fn not_required() -> Self {
        Self {
            policy: ReviewTiming::None,
            disposition: HandoffReviewDisposition::NotRequired,
        }
    }

    /// The review evidence this handoff carries, before-PR or
    /// before-landing, if any.
    pub fn evidence(&self) -> Option<&HandoffReviewEvidence> {
        match &self.disposition {
            HandoffReviewDisposition::BeforePr(evidence)
            | HandoffReviewDisposition::BeforeLanding(evidence) => Some(evidence.as_ref()),
            HandoffReviewDisposition::NotRequired => None,
        }
    }

    /// The timing the disposition records evidence for: `None` when it
    /// carries none.
    pub fn evidence_timing(&self) -> ReviewTiming {
        match &self.disposition {
            HandoffReviewDisposition::BeforePr(_) => ReviewTiming::BeforePr,
            HandoffReviewDisposition::BeforeLanding(_) => ReviewTiming::BeforeLanding,
            HandoffReviewDisposition::NotRequired => ReviewTiming::None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskHandoff {
    pub schema_version: u32,
    pub workspace_id: String,
    pub task_id: String,
    pub claim_id: String,
    pub machine_id: String,
    pub run_id: String,
    pub candidate: HandoffCandidate,
    pub review: HandoffReview,
    pub execution_summary: String,
    pub validation: Vec<HandoffArtifactRef>,
    /// Canonical relative additions outside the original module footprint.
    /// Recomputed from Git by the follower and independently by the owner.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub footprint_widening: Vec<String>,
    /// Criteria the task pilot typed post-deploy that this delivery owes
    /// until the deployed build is measured. The owner admits each against
    /// its own pilot finding when it accepts the handoff.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owed_criteria: Vec<super::OwedCriterion>,
    /// Culprit tasks the claimed implementer attributed this task's
    /// regression to (its `regression_from` output). A claimed worker cannot
    /// write its task's relations, so the owner checks each target against
    /// its own workspace and records the `regression_from` relations when it
    /// accepts the handoff.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regression_from: Vec<String>,
}

/// Captured command output in an owner-accessible task artifact, never an agent reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffValidationLog {
    pub schema_version: u32,
    pub workspace_id: String,
    pub task_id: String,
    pub claim_id: String,
    pub machine_id: String,
    pub run_id: String,
    pub candidate: HandoffCandidate,
    pub tested_head: String,
    pub command: String,
    pub exit_code: i32,
    pub output: String,
    /// [ORB-15427] Set on a failed command whose failing tests the leaf's
    /// own base run fails too. The owner accepts a non-zero log only with
    /// this report. When the host OSes match or either is unknown, its base
    /// run must share every failure named; when both known OSes differ, the
    /// owner checks base-branch history without running the base command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_red: Option<HandoffKnownRed>,
    /// Original failure and passing isolated replay on this exact candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flaky: Option<super::FlakyValidation>,
}

/// A claimed leaf's report that a required command failed only with tests
/// its base fails too [ORB-15427]. It is the leaf's claim, never proof. When
/// the host OSes match or either is unknown, the owner reruns the command on
/// this base before it accepts the log. When both OSes are known and differ,
/// the owner checks that the base is still on its captured base-branch history
/// and accepts the follower's reported failures for that OS without a base run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffKnownRed {
    /// The base commit the leaf compared with: the handed-off candidate's base.
    pub base_sha: String,
    /// What the candidate run reported testing, which a base run is handed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<serde_json::Value>,
    /// Every failure the candidate run's complete output names.
    pub failures: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedHandoff {
    pub handoff_id: String,
    pub handoff: TaskHandoff,
    /// Owner-required commands captured at acceptance, never chosen by the worker.
    pub required_commands: Vec<String>,
    pub accepted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HandoffAuthorizationSource {
    Operator,
    /// The owner's standing completion policy (`[workflow]
    /// distributed_completion = "done"`), recorded when the handoff was
    /// accepted and rechecked against the owner's configuration at landing.
    OwnerPolicy {
        reference: String,
    },
    /// Retained only so rows written before operation mode was removed still
    /// decode; completion under this source is always refused.
    Grant {
        grant_id: String,
    },
}

/// Immutable. Revocation is a separate durable record, not an overwrite of approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffAuthorization {
    pub authorization_id: String,
    pub handoff_id: String,
    pub workspace_id: String,
    pub task_id: String,
    pub claim_id: String,
    pub candidate: HandoffCandidate,
    pub approver: String,
    pub created_at: DateTime<Utc>,
    pub source: HandoffAuthorizationSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffRevocation {
    pub authorization_id: String,
    pub actor: String,
    pub reason: String,
    pub revoked_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingStartState {
    Pending,
    Revoked,
    /// The owner consumer verified this handoff's merge evidence and completed it.
    Completed,
}

/// Durable outbox consumed by the owner landing job. A pending request survives
/// restart without a drain or ship sweep; only verified completion or explicit
/// revocation settles it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingStartRequest {
    pub handoff_id: String,
    pub authorization_id: String,
    pub state: LandingStartState,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingAttemptState {
    /// One owner landing job owns this handoff until it merges or stops.
    Dispatched,
    /// Merged and completed against verified external evidence.
    Merged,
    /// Stopped with durable evidence; an operator may retry a healthy authorized
    /// handoff. A candidate repair needs fresh validation and a new handoff.
    Stopped,
}

/// The owner's durable record of landing work for one handoff. Handoff identity
/// deduplicates job creation: a second dispatch for a live attempt is refused
/// rather than starting a second merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingAttempt {
    pub handoff_id: String,
    pub authorization_id: String,
    pub task_id: String,
    pub claim_id: String,
    pub attempt: u32,
    pub state: LandingAttemptState,
    /// The owner-local job run carrying this attempt, once it is submitted. An
    /// attempt reserved before submission records it on the next dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// [ORB-16890] Times a task returns to the backlog because its handed-off
/// candidate fails its own required check at landing, after its one
/// automatic repair leaf, before the next such stop blocks it.
pub const LANDING_REPAIR_BUDGET: usize = 2;

/// History event of a landing stop that ended the claim and returned the
/// task to the backlog, keeping its candidate for the next run's repair.
pub const LANDING_REPAIR_REQUEUED_EVENT: &str = "landing_repair_requeued";

/// History event of the landing stop that found the task's landing repair
/// budget spent and blocked it. Requeues before it no longer count.
pub const LANDING_REPAIR_EXHAUSTED_EVENT: &str = "landing_repair_exhausted";

/// The landing repair requeues that count against a task's budget, from its
/// history event names in order: those after its last exhaustion, which an
/// operator answered by unblocking the task.
pub fn landing_repairs_spent<'a>(events: impl IntoIterator<Item = &'a str>) -> usize {
    events.into_iter().fold(0, |spent, event| match event {
        LANDING_REPAIR_REQUEUED_EVENT => spent + 1,
        LANDING_REPAIR_EXHAUSTED_EVENT => 0,
        _ => spent,
    })
}

/// An operator's compare-and-reserve request for the same immutable handoff.
/// No pipeline input or replacement authority is accepted by this operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LandingRetryRequest {
    pub handoff_id: String,
    pub expected_candidate_commit: String,
    pub expected_base_commit: String,
    pub expected_latest_attempt: u32,
    pub expected_latest_state: LandingAttemptState,
    pub reason: String,
    pub request_key: String,
}

/// Durable outbox receipt, committed with the reserved attempt before submission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingRetryReceipt {
    pub request: LandingRetryRequest,
    pub actor: String,
    pub attempt: u32,
    pub created_at: DateTime<Utc>,
}

/// Existing no-diff delivery contract shared with the local verifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlreadyLandedEvidence {
    pub schema_version: u32,
    pub task_id: String,
    pub run_id: String,
    pub tested_head: String,
    pub covering_commit: String,
    pub covering_task_id: String,
    pub scope: serde_json::Value,
    pub required_commands: Vec<String>,
    pub validation: Vec<AlreadyLandedCheck>,
    pub criteria_evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlreadyLandedCheck {
    #[serde(flatten)]
    pub validation: super::ReviewValidation,
    pub log_artifact: String,
}

/// A run's claim that its implementation correctly changed nothing. Bound to
/// the task, the run, and the pinned HEAD it validated; unlike
/// [`AlreadyLandedEvidence`] it names no covering commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoDiffEvidence {
    pub schema_version: u32,
    pub task_id: String,
    pub run_id: String,
    pub tested_head: String,
    pub reason: String,
    pub validation: Vec<NoDiffCheck>,
}

/// One validation command the no-diff run executed, with its exit status and
/// the task artifact holding the captured log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoDiffCheck {
    pub command: String,
    pub exit_code: i32,
    pub log_artifact: String,
}

/// A claimed implementer's outcome that its unchanged base already resolves
/// the task, naming the tests that show it [ORB-15747]. Those tests run only
/// outside the agent sandbox, so the claim host replays each on the base
/// before the NoDiff handoff: a failing replay fails the claim, and a
/// declaration that names nothing, or a selector that runs no test, never
/// settles done. It travels in the implementer's output as
/// [`ALREADY_RESOLVED_FIELD`], beside the clean-tree report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlreadyResolved {
    /// `cargo test -p <crate> --test <target> [<filter>]` selectors.
    pub selectors: Vec<String>,
}

/// The implementer output field that carries an [`AlreadyResolved`].
pub const ALREADY_RESOLVED_FIELD: &str = "already_resolved";

impl AlreadyResolved {
    /// The declaration in an implementer's `output`, if it makes one.
    pub fn from_output(output: &serde_json::Value) -> Result<Option<Self>, String> {
        match output.get(ALREADY_RESOLVED_FIELD) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(value) => serde_json::from_value(value.clone())
                .map(Some)
                .map_err(|error| {
                    format!(
                        "`{ALREADY_RESOLVED_FIELD}` must be `{{\"selectors\": [\"cargo test -p \
                         <crate> --test <target> [<filter>]\", ...]}}`: {error}"
                    )
                }),
        }
    }

    /// Admit every selector as one integration test target in the closed
    /// [`HostSandboxCommand`](super::HostSandboxCommand) grammar, since each
    /// runs on the claim host outside any agent sandbox. An exact required
    /// command is not a selector: validation already runs it, and it need not
    /// run a test.
    pub fn admit(&self) -> Result<Vec<super::HostSandboxCommand>, String> {
        if self.selectors.is_empty() {
            return Err(format!(
                "`{ALREADY_RESOLVED_FIELD}` names no test selectors, so nothing would show the \
                 base resolves the task"
            ));
        }
        let mut admitted: Vec<super::HostSandboxCommand> = Vec::new();
        for selector in &self.selectors {
            let command = super::HostSandboxCommand::admit(selector, &[]).map_err(|refusal| {
                format!(
                    "`{ALREADY_RESOLVED_FIELD}` selector `{selector}` is not admitted ({}): {}; \
                     a selector is `cargo test -p <crate> --test <target> [<filter>]`",
                    refusal.reason.as_str(),
                    refusal.detail
                )
            })?;
            if admitted.contains(&command) {
                return Err(format!(
                    "`{ALREADY_RESOLVED_FIELD}` names selector `{selector}` twice"
                ));
            }
            admitted.push(command);
        }
        Ok(admitted)
    }
}

pub fn already_landed_scope(
    task: &crate::task::Task,
    comments: &[crate::task::TaskComment],
) -> serde_json::Value {
    // A comment's scope is what it says; a receipt id is bookkeeping, and
    // leaving it out keeps one scope across Orbit versions.
    let comments = comments
        .iter()
        .map(|comment| {
            serde_json::json!({"at": comment.at, "by": comment.by, "message": comment.message})
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "title": task.title, "description": task.description,
        "acceptance_criteria": task.acceptance_criteria, "plan": task.plan,
        "context_files": task.context_files, "tags": task.tags, "relations": task.relations,
        "required_tools": task.required_tools, "type": task.task_type, "comments": comments,
    })
}
