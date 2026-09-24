//! Invocation-result types, error-code constants, and workflow-failure
//! helpers.

use orbit_common::text::floor_char_boundary;
use orbit_types::task::TaskStatus;
use orbit_types::telemetry::InvocationTrace;
use serde_json::Value;

use super::hosts::TaskAutomationUpdate;

pub const AGENT_INVOCATION_FAILED: &str = "AGENT_INVOCATION_FAILED";
pub const AGENT_TIMEOUT: &str = "AGENT_TIMEOUT";
pub const WORKFLOW_RUN_FAILED_EVENT: &str = "workflow_run_failed";

/// Maximum bytes of a run's `error_message` inlined verbatim into the
/// `workflow_run_failed` history note. Beyond this the note keeps the leading
/// bytes and points at the run record for the rest.
///
/// This is the only size threshold on the history-note surface and it is
/// declared exactly once on purpose — `scripts/check-history-note-size.sh`
/// fails the build if a second one appears, because a threshold copied into a
/// second writer is a threshold that drifts.
///
/// 1000 was chosen from the real distribution of `job_run_steps.error_message`
/// (497 recorded step errors, 2026-08-09): p50=183 B, p95=676 B, p99=14,720 B,
/// max=80,939 B. It leaves the p95 message fully inline and elides 18 of 497
/// (3.6%) — the genuine bulk only. [ORB-10343]
pub const MAX_NOTE_ERROR_BYTES: usize = 1_000;

pub fn workflow_failure_note(
    job_id: &str,
    run_id: &str,
    error_code: Option<&str>,
    error_message: Option<&str>,
) -> String {
    let error_code = error_code
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("-");
    let error_message = error_message
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("-");
    let error_message = elide_note_error(run_id, error_message);

    format!(
        "workflow run failed: job={job_id}, run_id={run_id}, error_code={error_code}, error={error_message}"
    )
}

/// Keep the head of an oversized `error_message` and replace the tail with the
/// command that reads the full text back.
///
/// This is not lossy truncation. `job_run_steps.error_message` persists the
/// whole message for the life of the run record, so the note was carrying a
/// duplicate of an already-durable value; the elision drops the copy, not the
/// original. The retrieval command is written into the note itself so a reader
/// who hits the elision does not have to know where run records live.
///
/// A worktree-integrity failure serializes its entire `dirty_paths` list into
/// this field, which is how one ORB-10332 note reached 85 KB — 8% of all task
/// history bytes in the workspace, in a single entry. [ORB-10343]
fn elide_note_error(run_id: &str, error_message: &str) -> String {
    if error_message.len() <= MAX_NOTE_ERROR_BYTES {
        return error_message.to_string();
    }
    let head = &error_message[..floor_char_boundary(error_message, MAX_NOTE_ERROR_BYTES)];
    let total = error_message.len();
    format!(
        "{head}… [elided: error_message is {total} B; full text: \
         `orbit run show {run_id} --json`, field .run.steps[].error_message]"
    )
}

pub fn blocked_workflow_failure_update(
    job_id: &str,
    run_id: &str,
    error_code: Option<&str>,
    error_message: Option<&str>,
) -> TaskAutomationUpdate {
    TaskAutomationUpdate {
        status: Some(TaskStatus::Blocked),
        status_event: Some(WORKFLOW_RUN_FAILED_EVENT.to_string()),
        status_note: Some(workflow_failure_note(
            job_id,
            run_id,
            error_code,
            error_message,
        )),
        ..TaskAutomationUpdate::default()
    }
}

#[derive(Debug, Clone)]
pub struct ActivityInvocationResult {
    pub response_json: Option<Value>,
    pub invocation_trace: InvocationTrace,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
}
