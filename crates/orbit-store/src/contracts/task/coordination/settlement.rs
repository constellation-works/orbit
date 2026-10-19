//! Claim settlement records and lifecycle mutation contracts.

use orbit_types::task::TaskStatus;
use serde::{Deserialize, Serialize};

use super::{
    AdmissionShipContract, ClaimEvidence, ClaimRun, ExecutionClaim, ExecutionClaimPhase,
    ExecutionLocation, PreservedClaimCandidate, TaskCoordinationRow,
};

/// A typed release the owner applied, kept on the claim's lifecycle state so
/// the task's release budget can count it [ORB-14257].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimReleaseRecord {
    pub class: orbit_types::workflow::ClaimFailureClass,
    pub reason: String,
    pub released_at: String,
    /// Set on the settlement that exhausted the budget and blocked the task;
    /// releases before it no longer count.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub budget_exhausted: bool,
    /// [ORB-14634] The release was for a forge outage
    /// ([`ClaimEvidence::forge_hold`]), which blames neither the drain's host
    /// nor its crew: admission does not keep the task from that drain.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub forge_unavailable: bool,
    /// [ORB-14695] The release was for a provider usage limit
    /// (`ClaimFailure::provider_limit`), which does not count against the
    /// task's release budget.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub provider_limit: bool,
    /// The release was for the owner's own busy store
    /// (`ClaimFailure::owner_busy`), which blames neither the drain's host
    /// nor its crew nor the task: admission does not keep the task or the
    /// host from that drain, and the release does not count against the
    /// task's release budget.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub owner_busy: bool,
}

impl ClaimReleaseRecord {
    /// Whether this release counts against the task's release budget.
    #[must_use]
    pub fn budgeted(&self) -> bool {
        self.class.budgeted() && !self.provider_limit && !self.owner_busy
    }
}

/// What a claimed leaf's failure or release settlement told the owner, kept
/// on the claim's lifecycle state [ORB-14439]. The leaf ran on another host,
/// so the owner's run history never records it; this is the owner's only
/// durable account of why it ended. Claims settled before the record existed
/// carry none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimSettlementRecord {
    /// The settlement the leaf sent. A release the owner turned into a
    /// failure still reads `release`; the claim's phase says what applied.
    pub kind: ClaimSettlementKind,
    /// The most specific evidence the settlement carried.
    pub evidence: ClaimSettlementEvidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_class: Option<orbit_types::workflow::ClaimFailureClass>,
    /// The typed failure's reason, else the settlement summary; bounded.
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    /// The first leaf step that did not complete, when the leaf kept a
    /// candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_step_id: Option<String>,
    pub settled_at: String,
}

/// Largest reason a [`ClaimSettlementRecord`] keeps.
const MAX_SETTLEMENT_REASON_BYTES: usize = 1024;

impl ClaimSettlementRecord {
    #[must_use]
    pub fn of(kind: ClaimSettlementKind, evidence: &ClaimEvidence, settled_at: String) -> Self {
        let failure = evidence.failure.as_ref();
        let reason = failure
            .map(|failure| failure.reason.as_str())
            .or(evidence.summary.as_deref())
            .unwrap_or_default()
            .trim();
        let cut = orbit_common::text::floor_char_boundary(reason, MAX_SETTLEMENT_REASON_BYTES);
        Self {
            kind,
            evidence: ClaimSettlementEvidence::of(evidence),
            failure_class: failure.map(|failure| failure.class),
            reason: reason[..cut].to_string(),
            crew: failure
                .and_then(|failure| failure.crew.clone())
                .or_else(|| {
                    evidence
                        .provider_unavailable
                        .as_ref()
                        .and_then(|provider| provider.crew.clone())
                }),
            failed_step_id: failure
                .and_then(|failure| failure.candidate.as_ref())
                .and_then(|candidate| candidate.failed_step_id.clone()),
            settled_at,
        }
    }
}

/// The status note of the owner's `claim_failed` history entry: what the
/// leaf's settlement told the owner, in the `key=value` shape of a run
/// failure's note (`workflow run failed: job=…, run_id=…, error=…`), so task
/// history and every reader of it see the cause. The settlement record stays
/// the typed source; claims failed before the note existed record none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimFailedNote {
    /// The machine the leaf ran on, by name when the claim recorded one.
    pub host: String,
    /// The leaf run, on the executing host's store; `None` when the claim
    /// failed before a leaf was bound.
    pub run_id: Option<String>,
    pub evidence: ClaimSettlementEvidence,
    pub failure_class: Option<orbit_types::workflow::ClaimFailureClass>,
    pub failed_step_id: Option<String>,
    /// The settlement's bounded reason, redacted when rendered.
    pub reason: String,
}

const CLAIM_FAILED_NOTE_PREFIX: &str = "claimed leaf failed: ";

impl ClaimFailedNote {
    #[must_use]
    pub fn of(
        record: &ClaimSettlementRecord,
        executed_on: &orbit_types::task::ExecutionLocation,
        run: Option<&ClaimRun>,
    ) -> Self {
        Self {
            host: executed_on
                .machine_name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| executed_on.machine_id.clone()),
            run_id: run.map(|run| run.run_id.clone()),
            evidence: record.evidence,
            failure_class: record.failure_class,
            failed_step_id: record.failed_step_id.clone(),
            reason: record.reason.clone(),
        }
    }

    /// The note text. Field values never contain the `, ` separator, and the
    /// reason, last, is redacted.
    #[must_use]
    pub fn text(&self) -> String {
        let value = |value: Option<&str>| {
            value
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map_or_else(|| "-".to_string(), |value| value.replace([',', ' '], "_"))
        };
        let reason = orbit_common::security::redaction::redact_all(self.reason.trim());
        format!(
            "{CLAIM_FAILED_NOTE_PREFIX}host={}, run_id={}, evidence={}, failure_class={}, \
             failed_step={}, error={}",
            value(Some(&self.host)),
            value(self.run_id.as_deref()),
            self.evidence.as_str(),
            value(self.failure_class.map(|class| class.as_str())),
            value(self.failed_step_id.as_deref()),
            if reason.is_empty() { "-" } else { &reason },
        )
    }

    /// Read a note [`Self::text`] wrote; `None` for any other note.
    #[must_use]
    pub fn parse(note: &str) -> Option<Self> {
        let rest = note.strip_prefix(CLAIM_FAILED_NOTE_PREFIX)?;
        let (header, reason) = rest.split_once(", error=")?;
        let mut fields = std::collections::BTreeMap::new();
        for field in header.split(", ") {
            let (key, value) = field.split_once('=')?;
            fields.insert(key, Some(value).filter(|value| *value != "-"));
        }
        let field = |key: &str| fields.get(key).copied().flatten().map(str::to_string);
        Some(Self {
            host: field("host").unwrap_or_default(),
            run_id: field("run_id"),
            evidence: ClaimSettlementEvidence::parse(fields.get("evidence").copied().flatten()?)?,
            failure_class: match field("failure_class") {
                Some(class) => Some(serde_json::from_value(serde_json::Value::String(class)).ok()?),
                None => None,
            },
            failed_step_id: field("failed_step"),
            reason: if reason == "-" {
                String::new()
            } else {
                reason.to_string()
            },
        })
    }
}

/// Which settlement a claimed leaf sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimSettlementKind {
    /// The leaf failed its claim; the owner blocks the task.
    Fail,
    /// The leaf gave its claim back unfinished.
    Release,
}

/// The evidence class of a claimed leaf's settlement, most specific first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimSettlementEvidence {
    /// [`ClaimEvidence::provider_unavailable`].
    ProviderUnavailable,
    /// [`ClaimEvidence::baseline_red`].
    BaselineRed,
    /// [`ClaimEvidence::forge_hold`].
    ForgeUnavailable,
    /// [`ClaimEvidence::evidence_hold`].
    EvidenceHold,
    /// [`ClaimEvidence::final_recovery`].
    FinalRecovery,
    /// Only a typed [`ClaimEvidence::failure`].
    Failure,
    /// Only a summary: an untyped settlement, such as a leaf that never
    /// launched because the owner refused its bind.
    Summary,
}

impl ClaimSettlementEvidence {
    #[must_use]
    pub fn of(evidence: &ClaimEvidence) -> Self {
        if evidence.provider_unavailable.is_some() {
            Self::ProviderUnavailable
        } else if evidence.baseline_red.is_some() {
            Self::BaselineRed
        } else if evidence.forge_hold.is_some() {
            Self::ForgeUnavailable
        } else if evidence.evidence_hold.is_some() {
            Self::EvidenceHold
        } else if evidence.final_recovery.is_some() {
            Self::FinalRecovery
        } else if evidence.failure.is_some() {
            Self::Failure
        } else {
            Self::Summary
        }
    }

    /// The class's wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderUnavailable => "provider_unavailable",
            Self::BaselineRed => "baseline_red",
            Self::ForgeUnavailable => "forge_unavailable",
            Self::EvidenceHold => "evidence_hold",
            Self::FinalRecovery => "final_recovery",
            Self::Failure => "failure",
            Self::Summary => "summary",
        }
    }

    /// The class a wire name names.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        [
            Self::ProviderUnavailable,
            Self::BaselineRed,
            Self::ForgeUnavailable,
            Self::EvidenceHold,
            Self::FinalRecovery,
            Self::Failure,
            Self::Summary,
        ]
        .into_iter()
        .find(|class| class.as_str() == name)
    }
}

/// A claimed leaf ended because its crew's provider could not be used on the
/// executing host (an authentication failure, for instance), not because the
/// work failed. Its claim is released to the backlog and the crew is excluded
/// on the executor: for the rest of its drain window after an authentication
/// failure, for a bounded backoff after a capacity failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderUnavailable {
    /// The crew the leaf ran as.
    pub crew: Option<String>,
    /// The provider's own diagnostic, bounded.
    pub reason: String,
}

/// [ORB-13907] A claimed leaf's final-recovery decision, carried to the owner
/// by the leaf's failure settlement instead of being written by the follower.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimFinalRecovery {
    /// The leaf run whose final recovery decided.
    pub run_id: String,
    /// The decision as the leaf's agent proposed it. The owner verifies a
    /// `complete_no_diff` commit against its own base branch.
    pub decision: orbit_types::workflow::FinalRecoveryDecision,
}

/// Worker-owned documents and coordination metadata. Lifecycle transitions
/// remain governed by the claim state machine, including typed review handoff.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimWorkerUpdate {
    pub evidence: ClaimEvidence,
    pub plan: Option<String>,
    pub context_files: Option<Vec<String>>,
    pub external_refs: Vec<orbit_types::task::ExternalRef>,
    pub status: Option<TaskStatus>,
    pub expected_status: Option<TaskStatus>,
    pub status_note: Option<String>,
}

/// Internal lifecycle operations. Typed handoffs validate owner observations and durable
/// evidence inside the ownership/phase boundary. Approval records completion authority;
/// no operation here executes an external merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaimMutation {
    Bind {
        run: ClaimRun,
        ship: AdmissionShipContract,
    },
    Evidence(ClaimEvidence),
    Update(ClaimWorkerUpdate),
    /// Friction allocation and publication share the claim's commit transaction.
    Friction(crate::contracts::FrictionAddParams),
    /// Legacy serialized shape retained for reading only; new writes are refused.
    Handoff(ClaimEvidence),
    AcceptHandoff(orbit_types::workflow::handoff::TaskHandoff),
    ApproveHandoff {
        handoff_id: String,
        candidate: orbit_types::workflow::handoff::HandoffCandidate,
    },
    RevokeHandoff {
        handoff_id: String,
        reason: String,
    },
    Fail(ClaimEvidence),
    /// Mid-run steering's watermark for the claim's bound leaf run
    /// (`docs/design/mid-run-steering/2_design.md` §3): the leaf records on
    /// the owner what each task-context prompt it built carried, so the
    /// owner's record, never the leaf's replica, is what its promotion gate
    /// reads. Only the bound leaf may send it, while its claim runs.
    Steering(ClaimSteeringRecord),
    /// The executor gives an unfinished claim back: the task returns to the
    /// backlog and the claim is revoked. The summary is the reason and is
    /// required; the comment, when present, is posted on the task.
    Release(ClaimEvidence),
    Recover {
        status: TaskStatus,
        reason: String,
    },
    /// Durable guard for the later external merge consumer. Only operators can record
    /// or reconcile intent; revocation cannot race past an unresolved intent.
    MergeIntent {
        intent_id: String,
        resolved: bool,
        evidence: String,
    },
    /// Reserve the single live landing attempt for a handoff, or record the
    /// owner job that carries the reserved attempt. Handoff identity is the
    /// deduplication key; a stopped attempt reopens as the next attempt. A
    /// dispatched attached attempt requires exact terminal owner-job evidence
    /// from the owner store, retained as a stop in the attempt history.
    DispatchLanding {
        handoff_id: String,
        job_run_id: Option<String>,
    },
    /// Atomically compare a stopped, terminal-job dispatched, or uncertain
    /// attempt and reserve its retry; caller-supplied state is never stop proof.
    RetryLanding(orbit_types::workflow::handoff::LandingRetryRequest),
    /// Owner-only durable base-update reservation; acceptance remains immutable.
    ReserveBaseUpdate(orbit_types::workflow::PrBaseUpdateReservation),
    /// Attach only the run carrying this receipt's exact reserved attempt.
    AttachLandingRetry {
        handoff_id: String,
        request_key: String,
        job_run_id: String,
    },
    /// Complete an authorized landing against verified external merge evidence.
    /// Refused while a merge intent is unresolved or the authority is stale.
    CompleteLanding {
        handoff_id: String,
        evidence: String,
    },
    /// Stop the live attempt with durable evidence, leaving the task in review.
    /// A `repairable` stop — the candidate conflicts with, or is stale
    /// against, its base — instead moves an original claim to
    /// `repair_pending`, and blocks the task when the claim is already its
    /// repair [ORB-14261]. A `requeue` stop — the candidate fails its own
    /// required check after that repair is spent — ends the claim and
    /// returns the task to the backlog with the candidate and the stop
    /// evidence kept for its next run, and blocks the task once its landing
    /// repair budget is spent [ORB-16890].
    StopLanding {
        handoff_id: String,
        reason: String,
        #[serde(default)]
        repairable: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        requeue: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimInspection {
    pub claim: ExecutionClaim,
    pub bound_run: Option<ClaimRun>,
    pub created_at: String,
    pub updated_at: String,
    pub last_event: String,
    pub age_seconds: Option<i64>,
    pub unresolved_merge_intent: Option<String>,
    pub landing_invalidated: bool,
    /// [ORB-14257] The typed failure release the owner applied to this claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<ClaimReleaseRecord>,
    /// [ORB-14257] The candidate this claim's failure or release preserved,
    /// offered to the task's next claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserved_candidate: Option<PreservedClaimCandidate>,
    /// [ORB-14439] The failure or release settlement the claim's leaf sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement: Option<ClaimSettlementRecord>,
    /// Mid-run steering's watermark for the claim's bound leaf run, recorded
    /// through [`ClaimMutation::Steering`]. It belongs to that run, and every
    /// settlement keeps it, so a later run continuing the claim's candidate
    /// starts from it rather than delivering the same comments again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steering: Option<orbit_types::workflow::SteeringWatermark>,
}

/// What a claimed leaf's task-context prompt carried, as the leaf records it
/// on its owner through [`ClaimMutation::Steering`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClaimSteeringRecord {
    /// The prompt carried no steering block; `newest` is the newest comment
    /// it included.
    Snapshot {
        step_id: String,
        newest: Option<orbit_types::workflow::CommentLogMark>,
        built_at: chrono::DateTime<chrono::Utc>,
    },
    /// The prompt led with a steering block carrying `delivered` in full and
    /// cutting `truncated` down. Every operator comment up to `through` has
    /// now reached one of the run's prompts in full. The owner records a
    /// `steering_delivered` task history event in the same commit.
    Delivery {
        step_id: String,
        through: Option<orbit_types::workflow::CommentLogMark>,
        delivered: Vec<orbit_types::workflow::CommentLogMark>,
        truncated: Vec<orbit_types::workflow::CommentLogMark>,
        built_at: chrono::DateTime<chrono::Utc>,
    },
}

/// A claimed leaf's steering records, read from its owner.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimSteeringReading {
    /// The watermark the leaf's own claim recorded; absent until its run
    /// built a task-context prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<orbit_types::workflow::SteeringWatermark>,
    /// The watermark of the task's most recent other claim that recorded
    /// one: the baseline for a leaf that carries that claim's candidate and
    /// built no prompt of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherited: Option<orbit_types::workflow::SteeringWatermark>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimMutationResult {
    pub claim_id: String,
    pub phase: ExecutionClaimPhase,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub friction: Option<orbit_types::record::FrictionRecord>,
}

/// SQL effects checked and published at the journal's existing commit point.
#[derive(Debug, Clone, Default)]
pub(crate) struct ClaimCommitEffects {
    pub replacements: Vec<(TaskCoordinationRow, TaskCoordinationRow)>,
    pub release_reservation: Option<String>,
    pub friction: Option<(crate::contracts::FrictionAddParams, String)>,
    pub execution_origin: Option<ExecutionLocation>,
    pub worker_update: Option<ClaimWorkerUpdate>,
    /// Culprit tasks an accepted handoff attributes the task's regression
    /// to, added to its relations as `regression_from` in this commit.
    pub regression_from: Vec<String>,
    /// The claim id and bound run a worker's evidence write was authenticated
    /// against, which the owner stamps on coverage it commits [ORB-15962].
    pub claimed_submission: Option<(String, ClaimRun)>,
}
