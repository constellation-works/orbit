//! Verification of review-filed defect findings before they are promoted.
//!
//! A review job files what it believes is a defect as a `bug` task tagged with
//! the review that found it. When the workspace sets `[review]
//! verify_findings = true`, the task may not be approved, promoted to
//! `backlog` nor admitted by a drain until `finding_verification_pipeline`
//! records a CONFIRMED verdict for it; with the setting off (the default)
//! verification state does not gate it. Either way, a REJECTED verdict moves
//! it to `rejected` with its evidence. The trusted record is the `finding_verified` history entry the
//! verification actor writes; its comment is the human-readable copy that
//! `orbit task show` prints. The design is
//! `docs/design/finding-verification/2_design.md`.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_common::security::release::sha256_hex;
use orbit_types::identity::{Crew, agent_family_from_cli, infer_agent_family_from_model};
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    Task, TaskComment, TaskComplexity, TaskHistoryEntry, TaskStatus, TaskType,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::TaskRecordUpdateParams;
use crate::OrbitRuntime;
use crate::application::job::crew_pools::{seeded_crew_ticket, weighted_draw};

/// The job that verifies review-filed findings.
pub const FINDING_VERIFICATION_JOB: &str = "finding_verification_pipeline";
/// The actor every verdict write is attributed to.
pub(crate) const FINDING_VERIFICATION_ACTOR: &str = "finding-verification";
/// The trusted history event carrying a verdict record.
const FINDING_VERIFIED_EVENT: &str = "finding_verified";
/// The status event of a rejection by verification.
const FINDING_REJECTED_EVENT: &str = "finding_rejected";
/// First line of the verdict comment; the verdict follows in lowercase.
const COMMENT_HEADER: &str = "finding-verification:";
/// The hold classification and admission reason of an unverified finding.
pub(crate) const UNVERIFIED_REVIEW_FINDING: &str = "unverified_review_finding";
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
pub(crate) const VERIFIABLE_STATUSES: [TaskStatus; 3] = [
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

/// Whether `task` still needs a verdict: a review finding in a verifiable
/// status whose latest verdict is not CONFIRMED for its current material.
pub(crate) fn needs_verification(task: &Task, history: &[TaskHistoryEntry]) -> bool {
    is_review_finding(task)
        && VERIFIABLE_STATUSES.contains(&task.status)
        && !latest_verification(history).is_some_and(|record| {
            record.verdict == FindingVerdict::Confirmed
                && record.finding_fingerprint.as_deref() == Some(&finding_fingerprint(task))
        })
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
#[serde(rename_all = "lowercase")]
pub enum FindingSeverity {
    Critical,
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

/// One cited location: `path:line` at the verified revision, and why it matters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingEvidence {
    pub location: String,
    pub note: String,
}

impl FindingEvidence {
    /// The repository-relative path and 1-based line `location` names.
    pub(crate) fn path_and_line(&self) -> Result<(&str, u32), String> {
        let (path, line) =
            self.location.trim().rsplit_once(':').ok_or_else(|| {
                format!("evidence location {:?} is not `path:line`", self.location)
            })?;
        let line = line
            .parse::<u32>()
            .ok()
            .filter(|line| *line > 0)
            .ok_or_else(|| {
                format!(
                    "evidence location {:?} has no positive line number",
                    self.location
                )
            })?;
        if path.is_empty()
            || path.starts_with('/')
            || path.split('/').any(|part| part == ".." || part.is_empty())
        {
            return Err(format!(
                "evidence location {:?} is not a repository-relative path",
                self.location
            ));
        }
        Ok((path, line))
    }
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

/// What one verifier returned for one task. The host owns the revision, the
/// crew and the tier, so none of them are part of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifierReport {
    pub task_id: String,
    pub verdict: FindingVerdict,
    pub confidence: VerdictConfidence,
    #[serde(default)]
    pub evidence: Vec<FindingEvidence>,
    pub reasoning: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reproduction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<FindingSeverity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection: Option<FindingRejection>,
}

impl VerifierReport {
    /// The structural contract of a verdict. A CONFIRMED verdict cites at
    /// least one `path:line` and rates severity; a REJECTED one names its
    /// reason, with the fixing commit or the duplicate task it needs.
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.reasoning.trim().is_empty() {
            return Err("reasoning must not be empty".into());
        }
        for evidence in &self.evidence {
            evidence.path_and_line()?;
        }
        match self.verdict {
            FindingVerdict::Confirmed => {
                if self.evidence.is_empty() {
                    return Err("a CONFIRMED verdict must cite `path:line` evidence".into());
                }
                if self.severity.is_none() {
                    return Err("a CONFIRMED verdict must rate severity".into());
                }
                if self.rejection.is_some() {
                    return Err("a CONFIRMED verdict carries no rejection".into());
                }
            }
            FindingVerdict::Rejected => {
                let rejection = self
                    .rejection
                    .as_ref()
                    .ok_or("a REJECTED verdict must name its rejection")?;
                if rejection.detail.trim().is_empty() {
                    return Err("rejection detail must not be empty".into());
                }
                let named = |value: &Option<String>| {
                    value
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
                };
                if rejection.reason == RejectionReason::AlreadyFixed && !named(&rejection.fixed_at)
                {
                    return Err("an `already_fixed` rejection must name `fixed_at`".into());
                }
                if rejection.reason == RejectionReason::Duplicate && !named(&rejection.duplicate_of)
                {
                    return Err("a `duplicate` rejection must name `duplicate_of`".into());
                }
            }
        }
        Ok(())
    }

    /// Whether this verdict is re-verified by the next tier's pool: a
    /// REJECTED verdict from the low tier, or any low-confidence verdict.
    pub(crate) fn escalates_from(&self, tier: TaskComplexity) -> bool {
        self.confidence == VerdictConfidence::Low
            || (self.verdict == FindingVerdict::Rejected && tier == TaskComplexity::Low)
    }
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
    pub severity: Option<FindingSeverity>,
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
    /// The record for `report`, the deciding verifier's, after `crews`.
    pub(crate) fn decided(
        report: &VerifierReport,
        crews: Vec<VerifierVerdict>,
        verified_sha: &str,
        finding_fingerprint: Option<&str>,
        base_branch: &str,
        run_id: Option<&str>,
    ) -> Self {
        Self {
            schema_version: FINDING_VERIFICATION_SCHEMA_VERSION,
            verdict: report.verdict,
            confidence: report.confidence,
            verified_sha: verified_sha.to_string(),
            finding_fingerprint: finding_fingerprint.map(ToOwned::to_owned),
            base_branch: base_branch.to_string(),
            severity: report.severity,
            evidence: report
                .evidence
                .iter()
                .map(|evidence| FindingEvidence {
                    location: evidence.location.trim().to_string(),
                    note: redact_all(&evidence.note),
                })
                .collect(),
            reasoning: redact_all(&report.reasoning),
            reproduction: report.reproduction.as_deref().map(redact_all),
            rejection: report.rejection.clone().map(|rejection| FindingRejection {
                detail: redact_all(&rejection.detail),
                ..rejection
            }),
            escalated: crews.len() > 1,
            crews,
            run_id: run_id.map(ToOwned::to_owned),
            verified_at: Utc::now(),
        }
    }

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
                    "finding rejected at {} ({reason}): {}",
                    self.verified_sha, rejection.detail
                )
            }
            None => format!("finding rejected at {}", self.verified_sha),
        }
    }
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

/// Whether `history` past `boundary` records a verdict another run wrote.
pub(crate) fn verified_since(history: &[TaskHistoryEntry], boundary: usize) -> bool {
    latest_verification(history.get(boundary..).unwrap_or_default()).is_some()
}

/// A review finding that may not be promoted or admitted yet.
pub(crate) struct UnverifiedFindingHold {
    task_id: String,
    latest: Option<FindingVerdict>,
}

impl UnverifiedFindingHold {
    pub(crate) fn of(task: &Task, history: &[TaskHistoryEntry]) -> Option<Self> {
        needs_verification(task, history).then(|| Self {
            task_id: task.id.clone(),
            latest: latest_verification(history).map(|record| record.verdict),
        })
    }

    pub(crate) fn detail(&self) -> String {
        let latest = match self.latest {
            Some(FindingVerdict::Rejected) => {
                " Its latest verdict is REJECTED; it was reopened without a new verification."
            }
            _ => "",
        };
        format!(
            "Unverified review finding: a review-filed bug is promoted or admitted only after `{FINDING_VERIFICATION_JOB}` records a CONFIRMED verdict for it.{latest} Run `orbit run job {FINDING_VERIFICATION_JOB} --input 'task_ids=[\"{}\"]'`, or enable the seeded `finding_verification.yaml` routine to verify it automatically. To accept the finding without verification, change its type from `bug` or remove its review tag; to stop gating review findings in this workspace, run `orbit config set review.verify_findings false`.",
            self.task_id
        )
    }
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct VerifierAssignment {
    pub(crate) crew: String,
    pub(crate) tier: TaskComplexity,
}

/// The tier a task is first verified at: its complexity, with an unassessed
/// task verified as medium, the complexity review findings are filed with.
pub(crate) fn verification_tier(task: &Task) -> TaskComplexity {
    match task.complexity {
        Some(TaskComplexity::Low) => TaskComplexity::Low,
        Some(TaskComplexity::Hard) => TaskComplexity::Hard,
        Some(TaskComplexity::XHard) => TaskComplexity::XHard,
        Some(TaskComplexity::Medium | TaskComplexity::Unassessed) | None => TaskComplexity::Medium,
    }
}

/// The tier that re-verifies a verdict from `tier`; none above `xhard`.
pub(crate) fn next_tier(tier: TaskComplexity) -> Option<TaskComplexity> {
    match tier {
        TaskComplexity::Low => Some(TaskComplexity::Medium),
        TaskComplexity::Medium | TaskComplexity::Unassessed => Some(TaskComplexity::Hard),
        TaskComplexity::Hard => Some(TaskComplexity::XHard),
        TaskComplexity::XHard => None,
    }
}

/// Whether `crew` is the one `label` names: by crew name, model, provider or
/// the agent family of either.
fn crew_named_by(crew: &Crew, label: &str) -> bool {
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
    /// pool. A crew `avoid` names — the filer, or a verifier that already
    /// judged the task — is passed over while the pool offers another. The
    /// draw is seeded by task and tier, so a retried preparation draws the
    /// same crew. `Ok(None)` when the tier has no enabled pool member.
    pub(crate) fn draw_finding_verifier(
        &self,
        task: &Task,
        tier: TaskComplexity,
        avoid: &[&str],
    ) -> Result<Option<VerifierAssignment>, OrbitError> {
        // A parked member (weight 0) holds no ticket.
        let candidates = self
            .configured_complexity_pool(tier)?
            .unwrap_or_default()
            .into_iter()
            .filter(|candidate| candidate.weight > 0)
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Ok(None);
        }
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
            seeded_crew_ticket(format!("{FINDING_VERIFICATION_JOB}:{}:{tier}", task.id));
        let selected = weighted_draw(&candidates, &source, &mut ticket)?;
        Ok(Some(VerifierAssignment {
            crew: selected.crew.name.clone(),
            tier,
        }))
    }

    /// The first verifier for `task`: its tier's pool, or the default crew
    /// when that pool names no enabled crew, as task creation falls back.
    pub(crate) fn first_finding_verifier(
        &self,
        task: &Task,
    ) -> Result<VerifierAssignment, OrbitError> {
        let tier = verification_tier(task);
        let filer = task.created_by.as_deref().unwrap_or_default();
        if let Some(assignment) = self.draw_finding_verifier(task, tier, &[filer])? {
            return Ok(assignment);
        }
        let crew = self.resolve_crew_for_task(None, None)?;
        Ok(VerifierAssignment {
            crew: crew.name,
            tier,
        })
    }

    /// Whether this workspace gates review findings on verification
    /// (`[review] verify_findings`, default off). Read from the config files
    /// on each check rather than from the settings this runtime opened with,
    /// so a dashboard toggle or `orbit config set` applies to a running drain,
    /// clock or dashboard without a restart.
    pub(crate) fn verifies_review_findings(&self) -> Result<bool, OrbitError> {
        let roots = orbit_config::ConfigRoots::new(self.global_root(), self.shared_root());
        Ok(orbit_config::ResolvedConfig::load(&roots)?
            .snapshot
            .review_verify_findings)
    }

    /// The hold that keeps `task` from promotion and from admission while it
    /// is a review finding without a CONFIRMED verdict and the workspace
    /// opted into [`Self::verifies_review_findings`]. Config and history are
    /// read only for a review finding in a verifiable status.
    pub(crate) fn unverified_finding_hold(
        &self,
        task: &Task,
    ) -> Result<Option<UnverifiedFindingHold>, OrbitError> {
        self.unverified_finding_hold_in(task, &|| self.get_task_history(&task.id))
    }

    /// [`Self::unverified_finding_hold`] reading history through `history`.
    pub(crate) fn unverified_finding_hold_in(
        &self,
        task: &Task,
        history: &dyn Fn() -> Result<Vec<TaskHistoryEntry>, OrbitError>,
    ) -> Result<Option<UnverifiedFindingHold>, OrbitError> {
        if !is_review_finding(task)
            || !VERIFIABLE_STATUSES.contains(&task.status)
            || !self.verifies_review_findings()?
        {
            return Ok(None);
        }
        Ok(UnverifiedFindingHold::of(task, &history()?))
    }

    /// Refuse to promote `task` while [`Self::unverified_finding_hold`] holds it.
    pub(crate) fn ensure_finding_verified(&self, task: &Task) -> Result<(), OrbitError> {
        match self.unverified_finding_hold(task)? {
            Some(hold) => Err(OrbitError::InvalidInput(hold.detail())),
            None => Ok(()),
        }
    }

    /// The latest verdict recorded for `task_id`.
    pub fn finding_verification(
        &self,
        task_id: &str,
    ) -> Result<Option<FindingVerification>, OrbitError> {
        Ok(latest_verification(&self.get_task_history(task_id)?))
    }

    /// Record `record` on `task_id`, prepared in `prepared_status` with
    /// `history_len` history entries: the trusted history entry, the verdict
    /// comment and, for REJECTED, the move to `rejected` — one write under
    /// the task lock. A task that left that status, is no longer a review
    /// finding, changed its material, or was verified by another run since is
    /// left untouched. Legacy prepared records without a fingerprint are stale.
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
                let updated = self.with_mutation(|| {
                    let updated = self.stores().task_records().update(
                        task_id,
                        TaskRecordUpdateParams {
                            actor: FINDING_VERIFICATION_ACTOR.to_string(),
                            status: rejected.then_some(TaskStatus::Rejected),
                            status_event: rejected.then(|| FINDING_REJECTED_EVENT.to_string()),
                            status_note: rejected.then(|| record.rejection_note()),
                            append_history: vec![TaskHistoryEntry {
                                at: now,
                                by: FINDING_VERIFICATION_ACTOR.to_string(),
                                event: FINDING_VERIFIED_EVENT.to_string(),
                                note: Some(note.clone()),
                                from_status: None,
                                to_status: None,
                                operation_id: None,
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
