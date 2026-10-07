//! Named external checks that hold a review without rejecting its candidate.

use serde::{Deserialize, Serialize};

use super::ValidationOutcome;
use super::automation::SourceRevision;
use super::host_evidence::EvidenceHostOs;
use crate::policy::{match_glob, normalize_glob_path};

/// Durable hold attached by review settlement, never an acceptance certificate.
pub const REVIEW_EVIDENCE_HOLD_ARTIFACT: &str = "review-evidence-hold.json";

/// The task history event that queues a held task for fresh review once every
/// named check arrived. Its note begins `run=<hold run>;`.
pub const REVIEW_EVIDENCE_RECEIVED_EVENT: &str = "review_evidence_received";

/// External environments whose evidence can arrive after the reviewer stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewEvidenceKind {
    HostedCi,
    NativeOs,
    #[serde(rename = "codeql")]
    CodeQl,
    /// [ORB-14478] A sandbox-gated test the agent lane cannot run because
    /// its own sandbox refuses a nested one. Names the host OS it needs; a
    /// host of that OS runs it outside the agent sandbox.
    HostSandboxTest,
}

/// One named external check and the task artifact where its result must arrive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewEvidenceRequirement {
    pub kind: ReviewEvidenceKind,
    pub name: String,
    pub command: String,
    pub artifact: String,
    /// The host OS the check must run on. Required for `host_sandbox_test`;
    /// when set, only evidence from that OS satisfies the requirement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<EvidenceHostOs>,
}

/// A hold pins the reviewed tree and task meaning; evidence never grants a pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewEvidenceHold {
    pub schema_version: u32,
    pub attempt_id: String,
    pub lineage_key: String,
    pub run_id: String,
    pub candidate: SourceRevision,
    pub task_meaning_digest: String,
    pub requirements: Vec<ReviewEvidenceRequirement>,
    /// [ORB-14450] The task's spec digest when the hold was written, so the
    /// next run resumes the held candidate only while the task's description
    /// and acceptance criteria are unchanged. Absent on older holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_spec_digest: Option<String>,
    /// The ref on `origin` a claimed leaf published the held candidate to,
    /// when that push succeeded. The owner keeps the candidate from it, so
    /// the task's next run resumes it instead of implementing again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_ref: Option<String>,
}

/// A workspace rule naming a check a claimed leaf's host owes for the paths
/// it changed, so Orbit synthesizes the requirement instead of relying on
/// the reviewer to name it. A `codeql` rule is owed by a host of another OS
/// and fulfilled by the owner on `os`; a `host_sandbox_test` rule is owed by
/// a host of `os`, which runs it outside the agent sandbox at settlement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HostEvidenceRule {
    pub kind: ReviewEvidenceKind,
    pub name: String,
    /// Workspace-relative globs; a changed path matching any of them owes
    /// the check.
    pub paths: Vec<String>,
    pub os: EvidenceHostOs,
    pub command: String,
    pub artifact: String,
}

impl HostEvidenceRule {
    /// The kinds a rule may name: those a host can produce without an agent.
    pub fn admits_kind(kind: ReviewEvidenceKind) -> bool {
        matches!(
            kind,
            ReviewEvidenceKind::CodeQl | ReviewEvidenceKind::HostSandboxTest
        )
    }

    /// Whether a host on `host` owes this rule's check rather than running
    /// it in the agent lane. `None` is a host OS no rule can name.
    pub fn owed_on(&self, host: Option<EvidenceHostOs>) -> bool {
        match self.kind {
            ReviewEvidenceKind::CodeQl => host != Some(self.os),
            ReviewEvidenceKind::HostSandboxTest => host == Some(self.os),
            ReviewEvidenceKind::HostedCi | ReviewEvidenceKind::NativeOs => false,
        }
    }

    /// Whether any of `changed` matches one of the rule's globs. A pattern
    /// or path that does not parse matches nothing; configuration loading
    /// refuses such a pattern.
    pub fn matches_any(&self, changed: &[String]) -> bool {
        changed.iter().any(|path| {
            normalize_glob_path(path).is_ok_and(|path| {
                self.paths
                    .iter()
                    .any(|pattern| match_glob(pattern, &path).unwrap_or(false))
            })
        })
    }

    pub fn requirement(&self) -> ReviewEvidenceRequirement {
        ReviewEvidenceRequirement {
            kind: self.kind,
            name: self.name.clone(),
            command: self.command.clone(),
            artifact: self.artifact.clone(),
            os: Some(self.os),
        }
    }
}

/// The requirements `rules` owe for a candidate that changed `changed` on a
/// host running `host`, in rule order.
pub fn owed_requirements(
    rules: &[HostEvidenceRule],
    changed: &[String],
    host: Option<EvidenceHostOs>,
) -> Vec<ReviewEvidenceRequirement> {
    rules
        .iter()
        .filter(|rule| {
            HostEvidenceRule::admits_kind(rule.kind)
                && rule.owed_on(host)
                && rule.matches_any(changed)
        })
        .map(HostEvidenceRule::requirement)
        .collect()
}

/// [ORB-14450] Evidence on an earlier candidate tree counted for this one:
/// the candidate was rebased onto a new base with its patch unchanged
/// (`git patch-id --stable` of the whole base..head diff).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewEvidenceCarried {
    pub from_tree: String,
    pub to_tree: String,
    pub patch_id: String,
}

/// Why evidence on an earlier candidate tree does not count for this one, so
/// the named checks are requested again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewEvidenceRerequestReason {
    /// The candidate's patch over its base differs from the one the
    /// evidence was checked on: a conflict resolution or another change.
    PatchChanged,
    /// The earlier candidate or its base is not readable here, so the
    /// patches cannot be compared.
    SourceUnavailable,
}

/// Evidence attached at a requirement's artifact path, with a separately
/// attached log. A matching passing result releases the hold for fresh review.
/// Attempt, commit and display name record provenance; identity is kind,
/// command and tree, so another review of the same tree can reuse the result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewExternalEvidence {
    pub schema_version: u32,
    pub attempt_id: String,
    pub candidate: SourceRevision,
    pub kind: ReviewEvidenceKind,
    pub name: String,
    pub command: String,
    pub outcome: ValidationOutcome,
    pub log_artifact: String,
    /// The host OS the check ran on. A requirement that names an OS counts
    /// only a result from that OS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<EvidenceHostOs>,
}

impl ReviewExternalEvidence {
    /// Whether this result names the check on the candidate tree, and on the
    /// host OS the requirement names, if any.
    /// The host must also validate the schema, outcome and attached log.
    pub fn matches_requirement(
        &self,
        requirement: &ReviewEvidenceRequirement,
        candidate: &SourceRevision,
    ) -> bool {
        !candidate.tree.is_empty()
            && self.candidate.tree == candidate.tree
            && self.kind == requirement.kind
            && self.command == requirement.command
            && requirement.os.is_none_or(|os| self.os == Some(os))
    }
}
