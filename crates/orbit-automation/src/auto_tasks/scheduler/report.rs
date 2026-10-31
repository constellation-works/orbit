use crate::auto_tasks::loader::AutoTaskLoadError;
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::{AutoTaskDefinition, AutoTaskPendingClaim, AutoTaskSkipRecord};

/// Per-definition outcome of one scheduler pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoTaskFireReport {
    /// Definition name.
    pub name: String,
    /// One of: `fired`, `would_fire`, `baselined`, `would_baseline`, `skipped`,
    /// `error`.
    pub action: &'static str,
    /// Why, for `skipped` rows; for `fired` rows, when the cursor checkpoint
    /// after minting failed or a pending mint was reconciled.
    pub reason: Option<String>,
    /// Scheduled slot consumed (RFC 3339, UTC), when a fire was involved.
    pub slot: Option<String>,
    /// Task minted by a fire.
    pub task_id: Option<String>,
    /// The still-open task id that caused a `dedupe_open` skip.
    pub blocking_task_id: Option<String>,
    pub automation: Option<orbit_types::workflow::automation::AutomationDiagnostic>,
}

/// Result of one scheduler pass.
#[derive(Debug, Default)]
pub struct AutoTaskSchedulerOutcome {
    /// Per-definition outcomes.
    pub reports: Vec<AutoTaskFireReport>,
    /// Fail-closed load failures (those definitions were absent this pass).
    pub errors: Vec<AutoTaskLoadError>,
}

/// Options for one scheduler pass.
#[derive(Debug, Clone, Copy, Default)]
pub struct SchedulerOptions {
    /// Report what would fire without recording or creating anything.
    pub dry_run: bool,
}

/// The audit row's reason: the token plus what it was decided on — the two
/// SHAs for a branch comparison, the recorded detail otherwise.
pub(super) fn skip_reason(record: &AutoTaskSkipRecord) -> String {
    if let Some(detail) = &record.detail {
        return format!("{}: {detail}", record.reason);
    }
    format!(
        "{}: {} tip {} is already covered by cursor {}{}",
        record.reason,
        record.reference,
        short_sha(&record.tip_sha),
        short_sha(&record.cursor_sha),
        record
            .cursor_task_id
            .as_ref()
            .map(|id| format!(" from {id}"))
            .unwrap_or_default()
    )
}

fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// Preview the same recovery outcome a live pass reports after checkpointing.
pub(super) fn pending_claim_report(
    definition: &AutoTaskDefinition,
    pending: &AutoTaskPendingClaim,
) -> AutoTaskFireReport {
    if let Some(task_id) = &pending.task_id {
        return AutoTaskFireReport {
            reason: Some(format!(
                "reconciled pending mint for slot {} as {task_id}",
                pending.slot
            )),
            slot: Some(pending.slot.clone()),
            task_id: Some(task_id.clone()),
            ..action(definition, "fired")
        };
    }

    AutoTaskFireReport {
        slot: Some(pending.slot.clone()),
        ..skipped(
            definition,
            &format!(
                "unresolved_pending: slot {} was claimed without mint evidence; inspect {} tagged tasks and auto-tasks.json before retrying — refusing to remint or consume the slot",
                pending.slot,
                orbit_types::workflow::auto_task_tag(&definition.name)
            ),
        )
    }
}

pub(super) fn skipped(definition: &AutoTaskDefinition, reason: &str) -> AutoTaskFireReport {
    AutoTaskFireReport {
        name: definition.name.clone(),
        action: "skipped",
        reason: Some(reason.to_string()),
        slot: None,
        task_id: None,
        blocking_task_id: None,
        automation: None,
    }
}

pub(super) fn action(definition: &AutoTaskDefinition, action: &'static str) -> AutoTaskFireReport {
    AutoTaskFireReport {
        name: definition.name.clone(),
        action,
        reason: None,
        slot: None,
        task_id: None,
        blocking_task_id: None,
        automation: None,
    }
}

pub(super) fn parse_rfc3339(raw: &str) -> Result<DateTime<Utc>, OrbitError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| OrbitError::Store(format!("invalid stored timestamp '{raw}': {error}")))
}
