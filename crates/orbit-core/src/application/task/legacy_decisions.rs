//! Readers for decisions recorded as text before [`TaskDecision`] existed.
//!
//! Every decision automation acts on is now a typed field on a task history
//! event. Records written before it carry the decision in text; these readers
//! are the only place that text is parsed, and each is consulted only where a
//! new record cannot reach it:
//!
//! - a final-recovery decision comment counts only for a run no typed record
//!   names. The applier writes both in one update, and only as `system`;
//! - an operator resolution event without a typed decision is read from its
//!   JSON note. Only the trusted operator update path writes those events;
//! - a `task-pilot-admission:` comment header and a `[drain-approval-held:`
//!   history-note marker count only when written before this store's typed
//!   decisions boundary. Their text is open to writers the decision must not
//!   trust (any comment author; a pilot agent's rationale).
//!
//! The boundary is per store: the first writable open of a store by a binary
//! that writes typed decisions records when that happened, beside the task
//! registry, and never moves it. Text an older binary wrote, on that host or
//! before it upgraded, predates the boundary; text written through this
//! binary follows it. A store with no recorded boundary (one only opened
//! read-only since) falls back to the day typed decisions shipped, which can
//! only refuse a legacy decision, never admit new text.
//!
//! Retire this module 30 days after the typed field shipped (from
//! 2026-10-12), once no live task holds a pre-typed decision.
//!
//! [`TaskDecision`]: orbit_types::task::TaskDecision

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::io::write_new_private_text;
use orbit_store::maintenance::task_registry::task_registry_path;
use orbit_types::task::{PilotAdmissionVerdict, TaskComment, TaskHistoryEntry};
use serde_json::Value;

use super::blocked_recovery::FinalRecoveryRecord;
use super::final_recovery::FinalRecoveryOutcome;
use super::helpers::{SYSTEM_ACTOR_LABEL, is_automation_actor};
use crate::OrbitRuntime;

/// The store's boundary, beside its task registry: an RFC 3339 instant.
const BOUNDARY_FILE: &str = "typed-decisions.since";

/// When typed decisions shipped (2026-10-12T00:00:00Z): the boundary of a
/// store that has none recorded.
fn typed_decisions_shipped() -> DateTime<Utc> {
    DateTime::from_timestamp(1_791_763_200, 0).unwrap_or(DateTime::<Utc>::MIN_UTC)
}

fn boundary_path(global_root: &Path) -> PathBuf {
    task_registry_path(global_root).with_file_name(BOUNDARY_FILE)
}

/// Record the store's typed-decisions boundary as now, unless one is already
/// recorded. Every writable open calls this before it can write a record, so
/// the first open by this binary fixes the boundary and later opens keep it.
pub(crate) fn record_typed_decisions_boundary(global_root: &Path) -> Result<(), OrbitError> {
    let path = boundary_path(global_root);
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true);
    match write_new_private_text(&path, &format!("{now}\n")) {
        Err(error) if error.kind() != ErrorKind::AlreadyExists => Err(OrbitError::io_with_context(
            &error,
            format!(
                "record typed decisions boundary '{}': {error}",
                path.display()
            ),
        )),
        _ => Ok(()),
    }
}

impl OrbitRuntime {
    /// When this store began recording decisions as typed fields. A text-form
    /// pilot override or drain hold written from then on is not a decision.
    pub(crate) fn typed_decisions_since(&self) -> DateTime<Utc> {
        std::fs::read_to_string(boundary_path(&self.global_root()))
            .ok()
            .and_then(|recorded| recorded.trim().parse().ok())
            .unwrap_or_else(typed_decisions_shipped)
    }
}

const FINAL_RECOVERY_PREFIX: &str = "final_recovery run_id=";
const DRAIN_HELD_PREFIX: &str = " [drain-approval-held:";

/// A legacy `final_recovery run_id=<run> decision=<kind> outcome=<outcome>`
/// comment the applier wrote as `system`.
pub(crate) fn final_recovery_comment(comment: &TaskComment) -> Option<FinalRecoveryRecord> {
    if comment.by != SYSTEM_ACTOR_LABEL {
        return None;
    }
    let header = comment.message.lines().next()?;
    if !header.starts_with(FINAL_RECOVERY_PREFIX) {
        return None;
    }
    Some(FinalRecoveryRecord {
        at: comment.at,
        run_id: header_field(header, "run_id=")?.to_string(),
        decision: header_field(header, "decision=")?.to_string(),
        outcome: header_field(header, "outcome=").unwrap_or("-").to_string(),
    })
}

/// The outcome a legacy decision comment for `run_id` records: its header's
/// `outcome=` (and a completion's `status=`), with the detail lines below it.
/// A comment the applier did not write as expected reads as an escalation,
/// which never settles the task twice.
pub(crate) fn final_recovery_outcome(
    comments: &[TaskComment],
    run_id: &str,
) -> Option<FinalRecoveryOutcome> {
    let marker = format!("{FINAL_RECOVERY_PREFIX}{run_id} decision=");
    let message = &comments
        .iter()
        .find(|comment| comment.message.starts_with(&marker))?
        .message;
    let header = message.lines().next().unwrap_or_default();
    let field = |key: &str| {
        header
            .split_whitespace()
            .find_map(|token| token.strip_prefix(key))
    };
    let detail = |key: &str| {
        message
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .map(|value| value.trim().to_string())
    };
    let escalated = FinalRecoveryOutcome::Escalated { reason: None };
    Some(match field("outcome=") {
        Some("completed") => {
            match (
                field("status=").and_then(|status| status.parse().ok()),
                detail("evidence_commit: "),
            ) {
                (Some(status), Some(evidence_commit)) => FinalRecoveryOutcome::Completed {
                    status,
                    evidence_commit,
                },
                _ => escalated,
            }
        }
        Some("rejected") => FinalRecoveryOutcome::Rejected,
        Some("archived") => FinalRecoveryOutcome::Archived,
        Some("requeued") => FinalRecoveryOutcome::Requeued,
        Some("refused") => FinalRecoveryOutcome::Refused {
            reason: detail("reason: ").unwrap_or_default(),
        },
        _ => escalated,
    })
}

/// A legacy operator `approve-anyway` or `clear` comment: its first line names
/// the verdict, an operator wrote it, and it predates `typed_since`, the
/// store's boundary (read only for a comment that otherwise qualifies).
pub(crate) fn pilot_override_comment(
    comment: &TaskComment,
    typed_since: &dyn Fn() -> DateTime<Utc>,
) -> bool {
    !comment.by.trim().is_empty()
        && !is_automation_actor(&comment.by)
        && PilotAdmissionVerdict::from_header(comment.message.lines().next().unwrap_or_default())
            .is_some_and(PilotAdmissionVerdict::overrides_findings)
        && comment.at < typed_since()
}

/// A legacy operator resolution event's JSON note: the pilot receipt it
/// answers, the hold it recorded, and whether it decided that hold (its
/// `decision` names a verdict and its evidence is a decision comment with a
/// body; a rescope records the hold without deciding it).
pub(crate) fn pilot_resolution_note(entry: &TaskHistoryEntry) -> Option<(String, Value, bool)> {
    let record: Value = serde_json::from_str(entry.note.as_deref()?).ok()?;
    let operation_id = record["operation_id"].as_str()?.to_string();
    let decided = record["decision"]
        .as_str()
        .and_then(PilotAdmissionVerdict::from_header)
        .is_some()
        && record["evidence"].as_str().is_some_and(|evidence| {
            let mut lines = evidence.lines();
            PilotAdmissionVerdict::from_header(lines.next().unwrap_or_default()).is_some()
                && lines.any(|line| !line.trim().is_empty())
        });
    Some((operation_id, record["hold"].clone(), decided))
}

/// The classification a legacy drain hold recorded in a `task_pilot_applied`
/// history note, when the note predates `typed_since`, the store's boundary
/// (read only for a note that carries the marker).
pub(crate) fn drain_held_classification<'a>(
    entry: &'a TaskHistoryEntry,
    typed_since: &dyn Fn() -> DateTime<Utc>,
) -> Option<&'a str> {
    let (_, marker) = entry.note.as_deref()?.split_once(DRAIN_HELD_PREFIX)?;
    let (classification, _) = marker.split_once(']')?;
    (entry.at < typed_since()).then_some(classification)
}

/// The `key=value` token in a comment header.
fn header_field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let start = text.find(key)? + key.len();
    let value = text[start..].split([',', ' ', ';', '\n']).next()?;
    (!value.is_empty() && value != "-").then_some(value)
}
