//! Verdicts on review-filed defect findings.
//!
//! A review job files what it believes is a defect as a `bug` task tagged with
//! the review that found it. The work that already reads its code may judge
//! the finding absent: the task pilot's `finding_invalid` result, or an
//! implementer's `finding-invalid.json` report
//! ([`OrbitRuntime::reject_invalid_finding`]). That REJECTED verdict moves the
//! task to `rejected` with its evidence. The trusted record is the
//! `finding_verified` history entry the verification actor writes; its comment
//! is the human-readable copy that `orbit task show` prints. Records the
//! retired `finding_verification_pipeline` wrote ([ORB-22844]) keep loading:
//! their verdict, crews and run id are read as before.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_common::security::release::sha256_hex;
use orbit_types::identity::{Crew, agent_family_from_cli, infer_agent_family_from_model};
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    DefectSeverity, SeveritySource, Task, TaskComment, TaskComplexity, TaskHistoryEntry,
    TaskRelation, TaskRelationType, TaskStatus, TaskType,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::TaskRecordUpdateParams;
use crate::OrbitRuntime;
use crate::application::contracts::FINDING_VERIFICATION_JOB;
use crate::application::contracts::InspectionPool;
use crate::application::task::{TaskJobPort, TaskRecords};

/// The actor every verdict write is attributed to.
pub(crate) const FINDING_VERIFICATION_ACTOR: &str = "finding-verification";
/// The trusted history event carrying a verdict record.
const FINDING_VERIFIED_EVENT: &str = "finding_verified";
/// The status event of a rejection by verification.
const FINDING_REJECTED_EVENT: &str = "finding_rejected";
/// First line of the verdict comment; the verdict follows in lowercase.
const COMMENT_HEADER: &str = "finding-verification:";
/// Version of [`FindingVerification`] persisted in history.
pub const FINDING_VERIFICATION_SCHEMA_VERSION: u32 = 1;

/// Tags a review job files its findings with.
pub const REVIEW_FINDING_TAGS: [&str; 4] = [
    "code-review",
    "delivery-code-review",
    "security-review",
    "qa-sweep",
];

/// Statuses in which a finding is verified and held: those before promotion
/// and the backlog a drain admits from.
const VERIFIABLE_STATUSES: [TaskStatus; 3] = [
    TaskStatus::Proposed,
    TaskStatus::Someday,
    TaskStatus::Backlog,
];

/// Whether `task` is a review-filed defect finding.
pub fn is_review_finding(task: &Task) -> bool {
    task.task_type == TaskType::Bug
        && task
            .tags
            .iter()
            .any(|tag| REVIEW_FINDING_TAGS.contains(&tag.trim()))
}

/// What a verdict judges: the finding as filed. A state trigger claims a
/// finding at this fingerprint, and a verdict is bound to it. An edit to any
/// of these fields releases the claim and invalidates confirmation; status,
/// comments and attribution are not part of it.
pub(crate) fn finding_fingerprint(task: &Task) -> String {
    let mut tags = task.tags.clone();
    tags.sort();
    sha256_hex(
        json!({
            "id": task.id, "title": task.title, "description": task.description,
            "acceptance_criteria": task.acceptance_criteria, "tags": tags,
            "task_type": task.task_type, "context_files": task.context_files,
        })
        .to_string()
        .as_bytes(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum FindingVerdict {
    Confirmed,
    Rejected,
}

impl FindingVerdict {
    fn label(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerdictConfidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionReason {
    /// The code does not behave the way the finding claims.
    PremiseWrong,
    /// The defect existed but the pinned revision no longer has it.
    AlreadyFixed,
    /// The defective path cannot be reached from any caller.
    Unreachable,
    /// Another task already tracks the same defect.
    Duplicate,
}

/// Which route decided a verdict. Records the job wrote before the other
/// routes existed carry no source and read as [`Self::Verification`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictSource {
    /// `finding_verification_pipeline`.
    #[default]
    Verification,
    /// The task pilot's `finding_invalid` result.
    TaskPilot,
    /// An implementer's `finding-invalid.json` report at its claim base.
    Implementer,
    /// A delivery's in-line visual verification, carried to the follow-up
    /// filed for a confirmed visual finding after landing. Visual review
    /// was removed [ORB-22807]: nothing writes this source any more, and it
    /// stays so verdicts recorded before then still load.
    VisualReview,
}

impl VerdictSource {
    fn is_verification(&self) -> bool {
        *self == Self::Verification
    }

    fn label(self) -> &'static str {
        match self {
            Self::Verification => "verification",
            Self::TaskPilot => "task-pilot",
            Self::Implementer => "implementer",
            Self::VisualReview => "visual-review",
        }
    }
}

/// One cited location, `path:line` at the verified revision, and why it
/// matters. A verdict recorded before visual review was removed
/// [ORB-22807] may cite an image region instead; it still loads, but a new
/// report must cite a source line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingEvidence {
    pub location: String,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingRejection {
    pub reason: RejectionReason,
    /// The commit that fixed it, for `already_fixed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed_at: Option<String>,
    /// The task that already tracks it, for `duplicate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<String>,
    pub detail: String,
}

/// One verifier's contribution to a recorded verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifierVerdict {
    pub crew: String,
    pub tier: TaskComplexity,
    pub verdict: FindingVerdict,
    pub confidence: VerdictConfidence,
}

/// The recorded verdict: the final verifier's report at the host-pinned
/// revision, with every crew that verified the task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingVerification {
    pub schema_version: u32,
    /// The route that decided it; absent for the verification job.
    #[serde(default, skip_serializing_if = "VerdictSource::is_verification")]
    pub source: VerdictSource,
    pub verdict: FindingVerdict,
    pub confidence: VerdictConfidence,
    /// The commit the verifiers inspected and the evidence cites.
    pub verified_sha: String,
    /// The prepared finding material this verdict judges. Legacy records
    /// without a fingerprint remain readable but cannot authorize admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finding_fingerprint: Option<String>,
    pub base_branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<DefectSeverity>,
    pub evidence: Vec<FindingEvidence>,
    pub reasoning: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reproduction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection: Option<FindingRejection>,
    /// Every verifier in order; the last one decided.
    pub crews: Vec<VerifierVerdict>,
    pub escalated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub verified_at: DateTime<Utc>,
}

impl FindingVerification {
    fn comment(&self) -> Result<String, OrbitError> {
        let record = serde_json::to_string_pretty(self)
            .map_err(|error| OrbitError::Execution(format!("encode verdict: {error}")))?;
        Ok(format!(
            "{COMMENT_HEADER} {}\n{record}",
            self.verdict.label()
        ))
    }

    fn rejection_note(&self) -> String {
        match &self.rejection {
            Some(rejection) => {
                let reason = serde_json::to_value(rejection.reason)
                    .ok()
                    .and_then(|value| value.as_str().map(ToOwned::to_owned))
                    .unwrap_or_default();
                format!(
                    "finding rejected by {} at {} ({reason}): {}",
                    self.source.label(),
                    self.verified_sha,
                    rejection.detail
                )
            }
            None => format!(
                "finding rejected by {} at {}",
                self.source.label(),
                self.verified_sha
            ),
        }
    }
}

/// A review finding the task pilot or an implementer judged absent at
/// `verified_sha`: the code does not do what the finding claims, or the
/// claimed consequence cannot happen. Recorded as a REJECTED verdict with
/// reason `premise_wrong`.
pub(crate) struct InvalidFinding<'a> {
    pub(crate) source: VerdictSource,
    /// The task state this judgement read.
    pub(crate) analyzed: AnalyzedFinding<'a>,
    pub(crate) verified_sha: &'a str,
    pub(crate) base_branch: &'a str,
    pub(crate) confidence: VerdictConfidence,
    /// Why the defect does not exist, with its `path:line` citations.
    pub(crate) detail: &'a str,
    /// Structured citations, when the route reports them separately.
    pub(crate) evidence: Vec<FindingEvidence>,
    /// The crew that judged it, when known.
    pub(crate) crew: Option<&'a str>,
    pub(crate) run_id: Option<&'a str>,
}

/// The task state a rejection route judged, carried from where the route
/// durably learned it rather than read again at the write: a fresh read would
/// bless whatever the task had become by then.
pub(crate) struct AnalyzedFinding<'a> {
    /// [`finding_fingerprint`] of the material judged.
    pub(crate) fingerprint: &'a str,
    /// The status the judgement expects the task still to have.
    pub(crate) status: TaskStatus,
    /// History length past which another run's verdict supersedes this one.
    /// `None` for a route whose status no other verdict can be written in.
    pub(crate) history_boundary: Option<usize>,
}

/// The latest verdict `history` records, read only from the verification
/// actor's own entries.
pub(crate) fn latest_verification(history: &[TaskHistoryEntry]) -> Option<FindingVerification> {
    history
        .iter()
        .rev()
        .filter(|entry| {
            entry.event == FINDING_VERIFIED_EVENT && entry.by == FINDING_VERIFICATION_ACTOR
        })
        .find_map(|entry| serde_json::from_str(entry.note.as_deref()?).ok())
}

/// `relations` with every classified `regression_from` link rated at the
/// CONFIRMED verdict's `severity`, source `verified`, so the verified value
/// is the one shown and scored. An operator's rating stands. `None` when no
/// link changes.
fn verified_relations(
    relations: &[TaskRelation],
    severity: DefectSeverity,
) -> Option<Vec<TaskRelation>> {
    let mut changed = false;
    let relations = relations
        .iter()
        .cloned()
        .map(|mut relation| {
            if relation.relation_type == TaskRelationType::RegressionFrom
                && let Some(regression) = relation.regression.as_mut()
                && regression.severity_source != SeveritySource::Operator
                && (regression.severity, regression.severity_source)
                    != (severity, SeveritySource::Verified)
            {
                regression.severity = severity;
                regression.severity_source = SeveritySource::Verified;
                changed = true;
            }
            relation
        })
        .collect();
    changed.then_some(relations)
}

/// Whether a verdict from `source` may be written on a task in `status`: an
/// implementer judges work it holds in progress, and every other route judges
/// a finding not yet started. Neither reaches a task that already settled.
fn verdict_admits_status(source: VerdictSource, status: TaskStatus) -> bool {
    match source {
        VerdictSource::Implementer => status == TaskStatus::InProgress,
        VerdictSource::Verification | VerdictSource::TaskPilot | VerdictSource::VisualReview => {
            VERIFIABLE_STATUSES.contains(&status)
        }
    }
}

/// The `Superseded` reason of a task whose status admits no verdict from its route.
pub(crate) const STATUS_NOT_VERIFIABLE: &str = "status_not_verifiable";

/// Whether `history` past `boundary` records a verdict another run wrote.
pub(crate) fn verified_since(history: &[TaskHistoryEntry], boundary: usize) -> bool {
    latest_verification(history.get(boundary..).unwrap_or_default()).is_some()
}

/// How a verdict write ended.
pub(crate) enum VerdictWrite {
    /// Recorded; the task now has this status.
    Recorded(TaskStatus),
    /// Not written: the task moved on since preparation.
    Superseded {
        reason: &'static str,
        detail: String,
    },
}

/// One verifier dispatched for a task: its crew and the tier it was drawn from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct VerifierAssignment {
    pub(crate) crew: String,
    pub(crate) tier: TaskComplexity,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) eligible_pool: Vec<orbit_types::workflow::ActivityCrewPoolMember>,
}

/// The tier a verdict's crew is recorded at: the task's complexity, with an
/// unassessed task recorded as medium, the complexity review findings are
/// filed with.
fn verification_tier(task: &Task) -> TaskComplexity {
    match task.complexity {
        Some(TaskComplexity::Low) => TaskComplexity::Low,
        Some(TaskComplexity::Hard) => TaskComplexity::Hard,
        Some(TaskComplexity::XHard) => TaskComplexity::XHard,
        Some(TaskComplexity::Medium | TaskComplexity::Unassessed) | None => TaskComplexity::Medium,
    }
}

/// Whether `crew` is the one `label` names: by crew name, model, provider or
/// the agent family of either.
pub(crate) fn crew_named_by(crew: &Crew, label: &str) -> bool {
    let label = label.trim().to_ascii_lowercase();
    if label.is_empty() {
        return false;
    }
    let provider = agent_family_from_cli(&crew.assignment.provider);
    crew.name.eq_ignore_ascii_case(&label)
        || crew.assignment.model.eq_ignore_ascii_case(&label)
        || provider == label
        || infer_agent_family_from_model(&label).is_some_and(|family| family == provider)
}

impl OrbitRuntime {
    /// The verifier for `task` at `tier`, drawn from that tier's complexity
    /// pool. `verify_finding` inspects the pinned source natively, so only
    /// crews whose provider has that surface are drawn, and a tier without
    /// one passes to the next tier up [ORB-15792]; the assignment names the
    /// tier drawn from. A crew `avoid` names — the filer, or a verifier that
    /// already judged the task — is passed over while the pool offers
    /// another. The draw is seeded by task and tier, so a retried preparation
    /// draws the same crew. `Ok(None)` when no tier from `tier` upward has an
    /// inspection-capable member with a ticket.
    pub(crate) fn draw_finding_verifier(
        &self,
        task: &Task,
        tier: TaskComplexity,
        avoid: &[&str],
    ) -> Result<Option<VerifierAssignment>, OrbitError> {
        let Some(InspectionPool {
            tier, candidates, ..
        }) = self.inspection_pool(tier)?
        else {
            return Ok(None);
        };
        let preferred = candidates
            .iter()
            .filter(|candidate| {
                !avoid
                    .iter()
                    .any(|label| crew_named_by(&candidate.crew, label))
            })
            .cloned()
            .collect::<Vec<_>>();
        let candidates = if preferred.is_empty() {
            candidates
        } else {
            preferred
        };
        let source = format!("pool:{tier}");
        let mut ticket =
            self.seeded_crew_draw_tickets(format!("{FINDING_VERIFICATION_JOB}:{}:{tier}", task.id));
        let draw = self.crew_pool_draw(candidates, &source, &mut *ticket)?;
        Ok(Some(VerifierAssignment {
            crew: draw.crew.name,
            tier,
            eligible_pool: draw.eligible_pool,
        }))
    }

    /// The latest verdict recorded for `task_id`.
    pub fn finding_verification(
        &self,
        task_id: &str,
    ) -> Result<Option<FindingVerification>, OrbitError> {
        Ok(latest_verification(&self.get_task_history(task_id)?))
    }

    /// Record that `task_id`'s review finding does not exist at
    /// `finding.verified_sha`: the same REJECTED verdict and move to
    /// `rejected` as [`Self::record_finding_verification`], attributed to the
    /// route that judged it and bound to `finding.analyzed`, so a task that
    /// changed meaning or status since is left untouched. A task that is not a
    /// review finding is refused. A replay of a rejection this run already
    /// recorded for the same material returns `Recorded(rejected)` without a
    /// second write.
    pub(crate) fn reject_invalid_finding(
        &self,
        task_id: &str,
        finding: &InvalidFinding<'_>,
    ) -> Result<VerdictWrite, OrbitError> {
        let task = self.get_task(task_id)?;
        if !is_review_finding(&task) {
            return Err(OrbitError::InvalidInput(format!(
                "task {task_id} is not a review-filed bug (type bug tagged one of {}), so no \
                 finding verdict can reject it",
                REVIEW_FINDING_TAGS.join(", ")
            )));
        }
        if finding.detail.trim().is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "task {task_id}: an invalid-finding rejection needs evidence"
            )));
        }
        let history = self.get_task_history(task_id)?;
        if task.status == TaskStatus::Rejected
            && latest_verification(&history).is_some_and(|record| {
                record.verdict == FindingVerdict::Rejected
                    && record.source == finding.source
                    && record.run_id.as_deref() == finding.run_id
                    && record.verified_sha == finding.verified_sha
                    && record.finding_fingerprint.as_deref() == Some(finding.analyzed.fingerprint)
            })
        {
            return Ok(VerdictWrite::Recorded(TaskStatus::Rejected));
        }
        let detail = redact_all(finding.detail.trim());
        let record = FindingVerification {
            schema_version: FINDING_VERIFICATION_SCHEMA_VERSION,
            source: finding.source,
            verdict: FindingVerdict::Rejected,
            confidence: finding.confidence,
            verified_sha: finding.verified_sha.to_string(),
            finding_fingerprint: Some(finding.analyzed.fingerprint.to_string()),
            base_branch: finding.base_branch.to_string(),
            severity: None,
            evidence: finding
                .evidence
                .iter()
                .map(|evidence| FindingEvidence {
                    location: evidence.location.trim().to_string(),
                    note: redact_all(&evidence.note),
                })
                .collect(),
            reasoning: detail.clone(),
            reproduction: None,
            rejection: Some(FindingRejection {
                reason: RejectionReason::PremiseWrong,
                fixed_at: None,
                duplicate_of: None,
                detail,
            }),
            crews: finding
                .crew
                .map(|crew| VerifierVerdict {
                    crew: crew.to_string(),
                    tier: verification_tier(&task),
                    verdict: FindingVerdict::Rejected,
                    confidence: finding.confidence,
                })
                .into_iter()
                .collect(),
            escalated: false,
            run_id: finding.run_id.map(ToOwned::to_owned),
            verified_at: Utc::now(),
        };
        self.record_finding_verification(
            task_id,
            &record,
            finding.analyzed.status,
            finding.analyzed.history_boundary.unwrap_or(history.len()),
        )
    }

    /// Record `record` on `task_id`, prepared in `prepared_status` with
    /// `history_len` history entries: the trusted history entry, the verdict
    /// comment and, for REJECTED, the move to `rejected`, or for CONFIRMED
    /// the verified severity on its `regression_from` links — one write
    /// under the task lock. A task that left that status, is in a status the
    /// record's route may not judge, is no longer a review finding, changed
    /// its material, or was verified by another run since is left untouched.
    /// Legacy prepared records without a fingerprint are stale.
    pub(crate) fn record_finding_verification(
        &self,
        task_id: &str,
        record: &FindingVerification,
        prepared_status: TaskStatus,
        history_len: usize,
    ) -> Result<VerdictWrite, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let note = serde_json::to_string(record)
            .map_err(|error| OrbitError::Execution(format!("encode verdict: {error}")))?;
        let comment = record.comment()?;
        let mut outcome = None;
        self.stores()
            .tasks()
            .with_task_write_lock(task_id, &mut || {
                let task = self.get_task(task_id)?;
                if task.status != prepared_status {
                    outcome = Some(VerdictWrite::Superseded {
                        reason: "status_changed",
                        detail: format!(
                            "task moved from {prepared_status} to {} during verification",
                            task.status
                        ),
                    });
                    return Ok(());
                }
                if !verdict_admits_status(record.source, task.status) {
                    outcome = Some(VerdictWrite::Superseded {
                        reason: STATUS_NOT_VERIFIABLE,
                        detail: format!(
                            "a {} verdict cannot be written on a {} task",
                            record.source.label(),
                            task.status
                        ),
                    });
                    return Ok(());
                }
                if !is_review_finding(&task) {
                    outcome = Some(VerdictWrite::Superseded {
                        reason: "not_a_review_finding",
                        detail: "task is no longer a review-filed bug".into(),
                    });
                    return Ok(());
                }
                if record.finding_fingerprint.as_deref() != Some(&finding_fingerprint(&task)) {
                    outcome = Some(VerdictWrite::Superseded {
                        reason: "material_changed",
                        detail: "finding material changed or its prepared fingerprint is missing"
                            .into(),
                    });
                    return Ok(());
                }
                if verified_since(&self.get_task_history(task_id)?, history_len) {
                    outcome = Some(VerdictWrite::Superseded {
                        reason: "verified_elsewhere",
                        detail: "another verification recorded a verdict during this run".into(),
                    });
                    return Ok(());
                }
                let now = Utc::now();
                let rejected = record.verdict == FindingVerdict::Rejected;
                let relations = match (record.verdict, record.severity) {
                    (FindingVerdict::Confirmed, Some(severity)) => {
                        verified_relations(&task.relations, severity)
                    }
                    _ => None,
                };
                let updated = self.with_mutation(|| {
                    let updated = self.stores().task_records().update(
                        task_id,
                        TaskRecordUpdateParams {
                            actor: FINDING_VERIFICATION_ACTOR.to_string(),
                            status: rejected.then_some(TaskStatus::Rejected),
                            status_event: rejected.then(|| FINDING_REJECTED_EVENT.to_string()),
                            status_note: rejected.then(|| record.rejection_note()),
                            relations,
                            append_history: vec![TaskHistoryEntry {
                                at: now,
                                by: FINDING_VERIFICATION_ACTOR.to_string(),
                                event: FINDING_VERIFIED_EVENT.to_string(),
                                note: Some(note.clone()),
                                from_status: None,
                                to_status: None,
                                operation_id: None,
                                decision: None,
                            }],
                            append_comments: vec![TaskComment {
                                at: now,
                                by: FINDING_VERIFICATION_ACTOR.to_string(),
                                message: comment.clone(),
                                operation_id: None,
                            }],
                            expected_status: Some(vec![prepared_status]),
                            ..Default::default()
                        },
                    )?;
                    let event = if rejected {
                        OrbitEvent::TaskProposalRejected {
                            id: task_id.to_string(),
                            rejected_by: FINDING_VERIFICATION_ACTOR.to_string(),
                        }
                    } else {
                        OrbitEvent::TaskUpdated {
                            id: task_id.to_string(),
                        }
                    };
                    Ok((updated, event))
                })?;
                outcome = Some(VerdictWrite::Recorded(updated.status));
                Ok(())
            })?;
        outcome.ok_or_else(|| {
            OrbitError::Execution("verdict write did not run under the task lock".into())
        })
    }
}
