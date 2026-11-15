//! An operator's waiver of a backlog task's readiness gaps: the
//! `readiness-waived` tag [ORB-25084].
//!
//! The tag waives every backlog readiness gap (`orbit_types::task::readiness`
//! defines which): missing context files, so the unknown-footprint rule no
//! longer holds the task for its pilot or an exclusive slot, and unassessed
//! complexity. Automatic selection then admits it like ordinary ready work.
//! Host OS, labels, browser kit, human-crew assignment, dependencies and real
//! lock holders are not readiness gaps, so the tag never lifts them.
//!
//! Adding the tag bypasses admission gates, so it is an operator act: any
//! other writer is refused, never stripped, and filing a task with it is
//! refused too. The operator's write records a `readiness_waived` history
//! entry naming the actor and the gaps waived. Removing the tag is open to
//! every writer, since it only restores the gates, and records
//! `readiness_waiver_removed` with the gaps that apply again.

use orbit_common::OrbitError;
use orbit_types::task::{
    READINESS_WAIVED_TAG, ReadinessStage, Task, TaskHistoryEntry, readiness_gaps, waives_readiness,
};
use serde_json::json;

use super::TaskUpdateParams;
use super::update::TaskFieldWriter;

/// History event an operator's readiness waiver records.
const READINESS_WAIVED_EVENT: &str = "readiness_waived";

/// History event the waiver's removal records.
const READINESS_WAIVER_REMOVED_EVENT: &str = "readiness_waiver_removed";

/// Refuse filing a task that already carries the waiver: the waiver is an
/// operator act on an existing task, recorded in its history.
pub(super) fn refuse_filed_readiness_waiver(tags: &[String]) -> Result<(), OrbitError> {
    if !waives_readiness(tags) {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "a new task cannot carry `{READINESS_WAIVED_TAG}`: the readiness waiver is an operator \
         act recorded in the task's history. File the task, then have an operator waive its \
         readiness gaps"
    )))
}

/// Refuse a writer other than an operator that adds the waiver (in the
/// normalized `tags` it would store).
pub(super) fn refuse_unauthorized_readiness_waiver(
    task: &Task,
    tags: &[String],
    writer: TaskFieldWriter,
) -> Result<(), OrbitError> {
    if writer == TaskFieldWriter::Operator
        || !waives_readiness(tags)
        || waives_readiness(&task.tags)
    {
        return Ok(());
    }
    Err(OrbitError::CapabilityDenied(format!(
        "only an operator can add `{READINESS_WAIVED_TAG}` to task {}: it waives the task's \
         readiness gaps, so a drain admits it like ready work. Ask an operator to waive them, \
         or close the gaps (context files, complexity)",
        task.id
    )))
}

/// The history entry for a write that adds or removes the waiver: the actor,
/// and the backlog gaps the written task has waived (or, on removal, that
/// apply again). `None` when the write leaves the tag as it was.
pub(super) fn readiness_waiver_history(
    task: &Task,
    params: &TaskUpdateParams,
    actor: &str,
) -> Option<TaskHistoryEntry> {
    let tags = params.tags.as_deref()?;
    let (before, after) = (waives_readiness(&task.tags), waives_readiness(tags));
    if before == after {
        return None;
    }
    let gaps = readiness_gaps(
        ReadinessStage::Backlog,
        tags,
        params
            .context_files
            .as_deref()
            .unwrap_or(&task.context_files),
        params.complexity.or(task.complexity),
    )
    .into_iter()
    .map(|gap| gap.code.as_str())
    .collect::<Vec<_>>();
    let (event, note) = if after {
        (
            READINESS_WAIVED_EVENT,
            json!({ "tag": READINESS_WAIVED_TAG, "waived": gaps }),
        )
    } else {
        (
            READINESS_WAIVER_REMOVED_EVENT,
            json!({ "tag": READINESS_WAIVED_TAG, "restored": gaps }),
        )
    };
    Some(TaskHistoryEntry {
        at: chrono::Utc::now(),
        by: actor.to_string(),
        event: event.to_string(),
        note: Some(note.to_string()),
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: None,
    })
}
