//! Run-block and claim-failure classification from history and settlements.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_store::contracts::{ClaimFailedNote, ClaimSettlementEvidence, ExecutionClaimPhase};
use orbit_types::identity::all_agent_families;
use orbit_types::task::{TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    BUBBLEWRAP_NAMESPACE_REFUSALS, ClaimFailureClass, FORGE_UNAVAILABLE_EXPIRED_EVENT,
    ProviderFailureClass,
};

use super::types::{Anchor, BlockCause};
use crate::OrbitRuntime;

/// Status events that put a task in `blocked` because a run judged the work:
/// the failure is attributed to the task's footprint unless its text names an
/// infrastructure cause. A before-PR review gate's escalation judged the
/// candidate too.
const RUN_BLOCK_EVENTS: &[&str] = &[
    "workflow_run_failed",
    "task_blocked_by_agent",
    "final_recovery_escalated",
    "review_gate_escalation",
];
const RUN_INTERRUPTED_EVENT: &str = "workflow_run_interrupted";
/// A claimed leaf's failure settlement, recorded on its owner.
const CLAIM_FAILED_EVENT: &str = "claim_failed";
/// The failure handoff of a run whose candidate could not be carried onto
/// its moved base, with the conflicting paths it recorded.
const CONFLICT_BLOCKED_EVENT: &str = "pr_conflict_blocked";
/// A run's failure handoff; a base conflict when the step that failed was
/// `sync_base`, a failure of the work otherwise.
const FAILURE_HANDOFF_EVENT: &str = "pr_failure_handoff";
const SYNC_BASE_STEP: &str = "sync_base";
/// Required validation could not find a tool: the host, not the candidate.
const VALIDATION_ENVIRONMENT_EVENT: &str = "validation_environment_blocked";
/// Status writes an operator, agent session or automation made by hand.
const MANUAL_BLOCK_EVENTS: &[&str] = &["status_changed", "claim_revoked", "forced"];
/// How far a claim settlement's time may stand from the `claim_failed` entry
/// it explains: both are stamped in the same owner commit.
const SETTLEMENT_MATCH_SECONDS: i64 = 300;

/// A history entry only a job run writes: worktree setup moving the task to
/// `in-progress`, or a run outcome naming its run.
pub(super) fn entry_marks_run(entry: &TaskHistoryEntry) -> bool {
    entry.event == "started"
        || entry.to_status == Some(TaskStatus::InProgress)
        || entry.note.as_deref().is_some_and(names_run)
}

/// Whether a note names the run that wrote it: `run_id=<id>` as a run
/// failure writes it, or `run=<id>` as a run's failure handoff does. A field
/// that merely ends in `run`, such as `source_run=`, does not count.
fn names_run(note: &str) -> bool {
    ["run_id=", "run="].iter().any(|key| {
        note.match_indices(key).any(|(start, _)| {
            !note[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        })
    })
}

/// What a claimed leaf's failure settlement recorded: from its owner's
/// `claim_failed` note, or for an entry older than that note, from the
/// claim's settlement record.
#[derive(Debug, Clone)]
pub(in crate::application) struct SettledFailure {
    /// Set when the settlement was matched by time rather than read off the
    /// entry; the entry it explains is the nearest.
    settled_at: Option<DateTime<Utc>>,
    evidence: String,
    failure_class: Option<ClaimFailureClass>,
    leaf_run: bool,
    reason: String,
}

/// Every failed claim's settlement, by task, for `claim_failed` entries
/// written before their note named the cause. An owner whose claims cannot
/// be read classifies those entries as unclassified rather than failing.
pub(in crate::application) fn failed_claim_settlements(
    runtime: &OrbitRuntime,
) -> BTreeMap<String, Vec<SettledFailure>> {
    let settlements = runtime
        .leaf_settlements(None, false)
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "codebase analytics: claim settlements unreadable");
            Vec::new()
        });
    let mut by_task: BTreeMap<String, Vec<SettledFailure>> = BTreeMap::new();
    for settlement in settlements {
        if settlement.phase != ExecutionClaimPhase::Failed {
            continue;
        }
        let Ok(settled_at) = DateTime::parse_from_rfc3339(&settlement.settled_at) else {
            continue;
        };
        by_task
            .entry(settlement.task_id)
            .or_default()
            .push(SettledFailure {
                settled_at: Some(settled_at.with_timezone(&Utc)),
                evidence: settlement.evidence.to_string(),
                failure_class: settlement.failure_class,
                leaf_run: settlement.leaf_run_id.is_some(),
                reason: settlement.reason.unwrap_or_default(),
            });
    }
    by_task
}

/// The settlement behind a `claim_failed` entry: the note it carries, else
/// the task's failed claim settled nearest to it.
fn settled_failure(
    entry: &TaskHistoryEntry,
    settlements: &[SettledFailure],
) -> Option<SettledFailure> {
    if let Some(note) = entry.note.as_deref().and_then(ClaimFailedNote::parse) {
        return Some(SettledFailure {
            settled_at: None,
            evidence: note.evidence.as_str().to_string(),
            failure_class: note.failure_class,
            leaf_run: note.run_id.is_some(),
            reason: note.reason,
        });
    }
    settlements
        .iter()
        .filter_map(|settlement| {
            let distance = (settlement.settled_at? - entry.at).num_seconds().abs();
            (distance <= SETTLEMENT_MATCH_SECONDS).then_some((distance, settlement))
        })
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, settlement)| settlement.clone())
}

/// The kind of actor a history entry's `by` label names: an agent family, an
/// Orbit automation (`system`, `system:*`), an unattributed write, or else a
/// person (`human:<user>`, `operator`, an `ORBIT_ACTOR` label, or the
/// machine of an operator's claim recovery).
pub(in crate::application) fn actor_kind(by: &str) -> &'static str {
    let by = by.trim();
    if all_agent_families().contains(&by) {
        "agent"
    } else if by == "system" || by.starts_with("system:") {
        "system"
    } else if by.is_empty() || by == "unknown" || by == "anonymous" {
        "unknown"
    } else {
        "operator"
    }
}

/// Classify one entry into `blocked` from its status event, its typed
/// fields and, where no typed field names the cause, its note. The note of a
/// run failure carries the run's `error_code` and error text, which the run
/// failure classification reads.
pub(in crate::application) fn classify_block(
    entry: &TaskHistoryEntry,
    settlements: &[SettledFailure],
) -> BlockCause {
    let event = entry.event.as_str();
    let text = entry.note.as_deref().unwrap_or("");
    match event {
        CLAIM_FAILED_EVENT => {
            return match settled_failure(entry, settlements) {
                Some(settled) => claim_cause(&settled),
                None => BlockCause::Unclassified,
            };
        }
        CONFLICT_BLOCKED_EVENT => return BlockCause::BaseConflict(conflicting_paths(text)),
        FAILURE_HANDOFF_EVENT if note_field(text, "failed_step=") == Some(SYNC_BASE_STEP) => {
            return BlockCause::BaseConflict(conflicting_paths(text));
        }
        VALIDATION_ENVIRONMENT_EVENT => return BlockCause::Infra("environment"),
        FORGE_UNAVAILABLE_EXPIRED_EVENT => return BlockCause::Infra("forge_unavailable"),
        _ if MANUAL_BLOCK_EVENTS.contains(&event) => {
            return BlockCause::Manual(actor_kind(&entry.by));
        }
        _ => {}
    }
    if let Some(cause) = infra_cause(text) {
        return BlockCause::Infra(cause);
    }
    if event == RUN_INTERRUPTED_EVENT {
        return BlockCause::Infra("interrupted");
    }
    if RUN_BLOCK_EVENTS.contains(&event) || event == FAILURE_HANDOFF_EVENT || names_run(text) {
        return BlockCause::Run;
    }
    BlockCause::Unclassified
}

/// A claimed leaf's failure by its settlement's evidence class, then its
/// typed failure class. The reason's text only tells apart the
/// infrastructure causes a typed provider or environment class leaves open.
fn claim_cause(settled: &SettledFailure) -> BlockCause {
    let refine = |allowed: &[&'static str], fallback: &'static str| {
        infra_cause(&settled.reason)
            .filter(|cause| allowed.contains(cause))
            .unwrap_or(fallback)
    };
    let provider = || {
        BlockCause::Infra(refine(
            &["auth", "sandbox", "provider_limit"],
            "provider_unavailable",
        ))
    };
    match ClaimSettlementEvidence::parse(&settled.evidence) {
        Some(ClaimSettlementEvidence::EvidenceHold) => return BlockCause::NotFailure,
        Some(ClaimSettlementEvidence::ProviderUnavailable) => return provider(),
        Some(ClaimSettlementEvidence::ForgeUnavailable) => {
            return BlockCause::Infra("forge_unavailable");
        }
        Some(ClaimSettlementEvidence::BaselineRed) => return BlockCause::Infra("baseline_red"),
        Some(ClaimSettlementEvidence::FinalRecovery) => return BlockCause::Run,
        Some(ClaimSettlementEvidence::Failure | ClaimSettlementEvidence::Summary) | None => {}
    }
    match settled.failure_class {
        Some(ClaimFailureClass::Candidate | ClaimFailureClass::TaskInput) => BlockCause::Run,
        Some(ClaimFailureClass::OperatorCancel) => BlockCause::NotFailure,
        Some(ClaimFailureClass::Provider) => provider(),
        Some(ClaimFailureClass::Environment) => {
            BlockCause::Infra(refine(&["auth", "sandbox", "disk"], "environment"))
        }
        Some(ClaimFailureClass::BaselineRed) => BlockCause::Infra("baseline_red"),
        Some(ClaimFailureClass::OwnerRoute) => BlockCause::Infra("owner_route"),
        Some(ClaimFailureClass::Transient) => BlockCause::Infra("transient"),
        Some(ClaimFailureClass::BaseConflict) => BlockCause::BaseConflict(Vec::new()),
        // An untyped settlement: the leaf's reason as a run failure's note
        // would read, when a leaf ran; one that never launched says nothing
        // about the work.
        None => match infra_cause(&settled.reason) {
            Some(cause) => BlockCause::Infra(cause),
            None if settled.leaf_run => BlockCause::Run,
            None => BlockCause::Unclassified,
        },
    }
}

/// `key<value>` in a `key=value, ` note, up to the next separator.
fn note_field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let start = text.find(key)? + key.len();
    let value = text[start..].split([',', ' ', ';', '\n']).next()?;
    (!value.is_empty() && value != "-").then_some(value)
}

/// The paths a failure handoff recorded as conflicting: its last field,
/// `conflicts=<path>, <path>` or `conflicts=none reported`.
fn conflicting_paths(text: &str) -> Vec<Anchor> {
    let Some((_, paths)) = text.rsplit_once("conflicts=") else {
        return Vec::new();
    };
    let mut anchors: Vec<Anchor> = paths
        .lines()
        .next()
        .unwrap_or_default()
        .split(", ")
        .filter(|path| *path != "none reported")
        .filter_map(Anchor::parse)
        .collect();
    anchors.sort();
    anchors.dedup();
    anchors
}

fn infra_cause(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    // A component or test name is not evidence of a failed infrastructure
    // operation. Match concrete diagnostics, outside identifier tokens.
    let any = |needles: &[&str]| {
        needles.iter().any(|needle| {
            let needle = needle.to_ascii_lowercase();
            lower.match_indices(&needle).any(|(start, matched)| {
                let identifier = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-');
                !lower[..start].chars().next_back().is_some_and(identifier)
                    && !lower[start + matched.len()..]
                        .chars()
                        .next()
                        .is_some_and(identifier)
            })
        })
    };
    if any(BUBBLEWRAP_NAMESPACE_REFUSALS)
        || any(&[
            "sandbox_apply: Operation not permitted",
            "sandbox-exec cannot apply a profile",
            "trusted sandbox-exec not available at",
            "Bubblewrap capability probe failed",
            "trusted Bubblewrap not available at",
            "bwrap: Creating new namespace failed:",
            "Orbit linux-bwrap policy denied the attempted write",
            "macOS sandbox hid the stored credential",
        ])
    {
        return Some("sandbox");
    }
    if any(&[
        "oauth session expired",
        "authentication failed",
        "provider authentication failure",
        "failed to authenticate",
        "authentication required",
        "not authenticated",
        "no authentication information found",
        "401 unauthorized",
        "http 401",
        "oauth token revoked",
        "invalid api key",
        "api key not valid",
        "login required",
        "please log in",
        "please login",
        "token expired",
        "expired token",
        "invalid token",
        "token has expired",
        "not logged in",
        "real login failure",
        "provider_unavailable/auth",
    ]) {
        return Some("auth");
    }
    if any(&["no space left", "enospc", "disk full", "disk quota"]) {
        return Some("disk");
    }
    let error_code = text
        .split_once("error_code=")
        .map(|(_, rest)| rest.split([',', ' ']).next().unwrap_or(""))
        .filter(|code| !code.is_empty() && *code != "-");
    match ProviderFailureClass::of(error_code, Some(text)) {
        // Terse provider responses need the typed infrastructure evidence:
        // these tokens alone also occur as symbols in repository failures.
        Some(ProviderFailureClass::Unavailable)
            if any(&[
                "unauthorized",
                "unauthenticated",
                "authentication_error",
                "invalid_api_key",
            ]) =>
        {
            Some("auth")
        }
        Some(ProviderFailureClass::Limit | ProviderFailureClass::Capacity) => {
            Some("provider_limit")
        }
        Some(ProviderFailureClass::Unavailable | ProviderFailureClass::Refusal) => {
            Some("provider_unavailable")
        }
        None => None,
    }
}
