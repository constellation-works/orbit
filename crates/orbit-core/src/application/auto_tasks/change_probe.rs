//! The `skip_if_unchanged` precondition's evidence [ORB-12698].
//!
//! A periodic sweep whose whole job is "review/validate what landed" has
//! nothing to do while the integration branch is quiet, yet `dedupe` only
//! answers "is a prior instance still open". This module answers the other
//! question — "did anything land since the last completed sweep" — from two
//! durable facts: the branch tip, and the structured cursor the last completed
//! sweep recorded as a task artifact. Prose in an execution summary is never
//! consulted.
//!
//! Every inconclusive outcome is [`ChangeProbe::Unknown`], which mints: a
//! precondition that cannot be answered must not be able to stop a sweep.

use orbit_automation::auto_tasks::scheduler::ChangeProbe;
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskStatus, TaskType};
use orbit_types::workflow::{SWEEP_CURSOR_ARTIFACT, SkipIfUnchanged, SweepCursorRecord};

use crate::OrbitRuntime;
use crate::application::automation::source::Source;

/// Resolve the precondition against this workspace's checkout and task store.
pub(crate) fn probe_change_since_last_sweep(
    runtime: &OrbitRuntime,
    precondition: &SkipIfUnchanged,
) -> Result<ChangeProbe, OrbitError> {
    let source = Source::new(&runtime.paths().repo_root);
    let tip = match source.verify_branch(&precondition.reference) {
        Ok(revision) => revision.commit,
        Err(error) => {
            return Ok(unknown(format!(
                "ref '{}' did not resolve: {error}",
                precondition.reference
            )));
        }
    };

    let Some(sweep) = newest_completed_sweep(runtime, precondition)? else {
        return Ok(unknown(
            "no completed sweep matches the cursor tags".to_string(),
        ));
    };
    let sweep_id = sweep.id.to_string();

    let record = match read_cursor(runtime, &sweep_id) {
        Ok(Some(record)) => record,
        Ok(None) => {
            return Ok(unknown(format!(
                "completed sweep {sweep_id} recorded no {SWEEP_CURSOR_ARTIFACT}"
            )));
        }
        Err(reason) => return Ok(unknown(format!("{sweep_id}: {reason}"))),
    };

    // A cursor taken on another branch says nothing about this ref's tip.
    if record.reference != precondition.reference {
        return Ok(unknown(format!(
            "cursor from {sweep_id} is for ref '{}', not '{}'",
            record.reference, precondition.reference
        )));
    }

    let cursor = match source.revision(&record.cursor) {
        Ok(revision) => revision.commit,
        Err(error) => {
            return Ok(unknown(format!(
                "cursor commit '{}' from {sweep_id} did not resolve: {error}",
                record.cursor
            )));
        }
    };

    // Both revisions are verified commits here, so a non-zero `--is-ancestor`
    // is git's answer ("not covered"), not a failure to answer.
    let covered = tip == cursor
        || source
            .git(&["merge-base", "--is-ancestor", &tip, &cursor])
            .is_ok();

    Ok(if covered {
        ChangeProbe::Unchanged {
            cursor,
            tip,
            cursor_task_id: Some(sweep_id),
        }
    } else {
        ChangeProbe::Changed { cursor, tip }
    })
}

fn unknown(reason: String) -> ChangeProbe {
    ChangeProbe::Unknown { reason }
}

/// The sweep whose cursor applies, selected exactly as the sweep templates
/// describe: newest `done` chore carrying every tag, `legacy_tags` consulted
/// only when the current tags select nothing, newer `created_at` winning and
/// the lexicographically smaller id breaking a tie.
pub(super) fn newest_completed_sweep(
    runtime: &OrbitRuntime,
    precondition: &SkipIfUnchanged,
) -> Result<Option<Task>, OrbitError> {
    if let Some(task) = newest_completed_tagged(runtime, &precondition.cursor.tags)? {
        return Ok(Some(task));
    }
    if precondition.cursor.legacy_tags.is_empty() {
        return Ok(None);
    }
    newest_completed_tagged(runtime, &precondition.cursor.legacy_tags)
}

fn newest_completed_tagged(
    runtime: &OrbitRuntime,
    tags: &[String],
) -> Result<Option<Task>, OrbitError> {
    if tags.is_empty() {
        return Ok(None);
    }
    Ok(select_sweep(runtime.list_tasks_by_tags(tags)?))
}

/// The selection itself, over already tag-matched candidates: completed chores
/// only — a `bug` finding sharing the sweep's tags is never a cursor — newest
/// `created_at` first, the lexicographically smaller id breaking a tie.
pub(super) fn select_sweep(mut candidates: Vec<Task>) -> Option<Task> {
    candidates.retain(|task| task.status == TaskStatus::Done && task.task_type == TaskType::Chore);
    candidates.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.id.as_str().cmp(right.id.as_str()))
    });
    candidates.into_iter().next()
}

/// Read and validate one sweep's cursor artifact. `Ok(None)` means the sweep
/// recorded none; `Err` carries why an existing one is unusable.
pub(super) fn read_cursor(
    runtime: &OrbitRuntime,
    task_id: &str,
) -> Result<Option<SweepCursorRecord>, String> {
    let artifact = runtime
        .get_task_artifact(task_id, SWEEP_CURSOR_ARTIFACT)
        .map_err(|error| format!("{SWEEP_CURSOR_ARTIFACT} unreadable: {error}"))?;
    let Some(artifact) = artifact else {
        return Ok(None);
    };
    let record: SweepCursorRecord = serde_json::from_slice(&artifact.content)
        .map_err(|error| format!("{SWEEP_CURSOR_ARTIFACT} is not a cursor record: {error}"))?;
    record
        .validate()
        .map_err(|error| format!("{SWEEP_CURSOR_ARTIFACT} rejected: {error}"))?;
    Ok(Some(record))
}
