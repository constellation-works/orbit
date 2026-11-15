//! [ORB-23812] A pinned head Orbit pushed that the forge never started a
//! required check run for.
//!
//! GitHub can drop a push's `pull_request` event. The head then reports no
//! workflow run at all, and no run, queued or otherwise, of any required
//! check, and a completion would wait out its whole budget for checks that
//! never start. Once that state has lasted
//! [`ABSENT_CHECK_RUN_GRACE_SECONDS`] on the head this run published,
//! completion re-requests hosted checks once through the forge adapter,
//! recording the request in the task history before it is made. The history
//! entry bounds the re-trigger to one per pull request head across every
//! completion attempt. Checks that start afterwards are watched as usual; if
//! none has started [`RETRIGGER_WAIT_SECONDS`] after the request, completion
//! holds as [`DeliveryRefusal::CheckRunsAbsent`]. Absent checks are never
//! passing, no merge is requested on them, and no repair is spent.
//!
//! [ORB-24457] A workflow run the forge reports for the head, in any state,
//! is checks still pending. A queued run has no check runs yet; completion
//! keeps watching it under the ordinary completion budget. The grace, the
//! one re-trigger and the hold apply only when the adapter reports no
//! workflow run for the head at all.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::task::TaskHistoryEntry;
use orbit_types::workflow::DeliveryRefusal;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::context::{RuntimeHost, TaskAutomationUpdate};

use super::super::super::operations;
use super::super::answers::PullRequestStatus;

/// How long a published head may report no run of any required check
/// before completion re-requests its checks.
pub(super) const ABSENT_CHECK_RUN_GRACE_SECONDS: u64 = 5 * 60;

/// How long after the re-trigger a required check run may still start
/// before completion holds.
pub(super) const RETRIGGER_WAIT_SECONDS: u64 = 5 * 60;

/// Task event recording one check re-trigger for a pull request head,
/// written before the forge is asked.
pub(super) const CHECKS_RETRIGGER_EVENT: &str = "pr_checks_retrigger_requested";

/// The history note of [`CHECKS_RETRIGGER_EVENT`].
#[derive(Debug, Serialize, Deserialize)]
struct RetriggerRecord {
    pr_number: String,
    head_sha: String,
    requested_at_ms: u64,
    run_id: Option<String>,
}

/// One completion attempt's view of the absent-check-run state.
#[derive(Default)]
pub(super) struct AbsentCheckRuns {
    /// The head first observed with no required check run, and the waited
    /// second of that observation.
    since: Option<(String, u64)>,
    /// The forge's answer to this attempt's re-trigger, for the hold message.
    outcome: Option<String>,
}

/// One poll's absent-check-run observation.
pub(super) struct AbsentCheckRunPoll<'a> {
    pub(super) task_ids: &'a [String],
    pub(super) run_id: Option<&'a str>,
    pub(super) workspace_path: &'a str,
    pub(super) pr_number: &'a str,
    pub(super) status: &'a PullRequestStatus,
    /// The head this run published, or `None` when it recorded no push.
    pub(super) published_head: Option<&'a str>,
    /// Whether this poll observed no run of any required check.
    pub(super) absent: bool,
    /// Whether the forge adapter reported a workflow run for this head.
    /// Any state counts as checks still pending, not a dropped event.
    pub(super) workflow_run_present: bool,
    /// The required checks the head has not reported, for messages.
    pub(super) missing_note: String,
    pub(super) waited_seconds: u64,
    pub(super) last_poll: bool,
}

impl AbsentCheckRuns {
    /// Re-trigger once, or hold, when the published head has had no workflow
    /// run and no run of any required check for long enough. A present
    /// workflow run keeps the ordinary wait. `Ok` keeps that wait too.
    pub(super) fn observe<H: RuntimeHost + ?Sized>(
        &mut self,
        host: &H,
        poll: &AbsentCheckRunPoll<'_>,
    ) -> Result<(), OrbitError> {
        let head = match poll.status.head_sha.as_deref() {
            Some(head)
                if poll.absent
                    && !poll.workflow_run_present
                    && poll.published_head == Some(head) =>
            {
                head
            }
            _ => {
                self.since = None;
                return Ok(());
            }
        };
        let since = match &self.since {
            Some((observed, since)) if observed == head => *since,
            _ => {
                self.since = Some((head.to_owned(), poll.waited_seconds));
                poll.waited_seconds
            }
        };
        let now_ms = super::super::super::completion_deadline::now_ms()?;
        if let Some(record) = recorded_retrigger(host, poll.task_ids, poll.pr_number, head)? {
            let waited_ms = now_ms.saturating_sub(record.requested_at_ms);
            if waited_ms >= RETRIGGER_WAIT_SECONDS * 1000 || poll.last_poll {
                return Err(self.hold(poll, head, waited_ms / 1000));
            }
            return Ok(());
        }
        if poll.waited_seconds.saturating_sub(since) < ABSENT_CHECK_RUN_GRACE_SECONDS {
            return Ok(());
        }
        let record = RetriggerRecord {
            pr_number: poll.pr_number.to_owned(),
            head_sha: head.to_owned(),
            requested_at_ms: now_ms,
            run_id: poll.run_id.map(ToOwned::to_owned),
        };
        record_retrigger(host, poll.task_ids, &record)?;
        let answer = host.run_private_vcs_operation(
            operations::PR_RETRIGGER_CHECKS,
            json!({
                "workspace_path": Path::new(poll.workspace_path).to_string_lossy(),
                "pr": poll.pr_number,
                "head_sha": head,
            }),
        );
        self.outcome = Some(match answer {
            Ok(answer) if answer["requested"] == true => "the forge accepted it".to_owned(),
            Ok(answer) => format!(
                "the forge was not asked: {}",
                answer["reason"].as_str().unwrap_or("no reason given")
            ),
            Err(error) => {
                tracing::warn!(%error, pr = poll.pr_number, head, "check re-trigger failed");
                format!("the request failed: {error}")
            }
        });
        Ok(())
    }

    fn hold(&self, poll: &AbsentCheckRunPoll<'_>, head: &str, waited: u64) -> OrbitError {
        let outcome = self
            .outcome
            .as_deref()
            .map(|outcome| format!(" ({outcome})"))
            .unwrap_or_default();
        OrbitError::DeliveryRefused {
            refusal: DeliveryRefusal::CheckRunsAbsent,
            message: format!(
                "pr_complete: pull request #{pr} head {head} has no workflow run and no run of \
                 any required check {waited}s after its one check re-trigger{outcome}{missing}; \
                 the head is unchanged, no repair was spent and the task stays in review. Next \
                 step: an auto-fix pass hands the task back once head {head} reports a required \
                 check run. If none appear, confirm the forge runs workflows for this \
                 repository, then start the required checks on that head by hand or push a new \
                 head",
                pr = poll.pr_number,
                missing = poll.missing_note,
            ),
        }
    }
}

/// The re-trigger already recorded for `head` of pull request `pr` on any
/// of `task_ids`.
fn recorded_retrigger<H: RuntimeHost + ?Sized>(
    host: &H,
    task_ids: &[String],
    pr: &str,
    head: &str,
) -> Result<Option<RetriggerRecord>, OrbitError> {
    for task in task_ids {
        let found = host
            .get_task_history(task)?
            .iter()
            .filter(|entry| entry.event == CHECKS_RETRIGGER_EVENT)
            .filter_map(|entry| {
                serde_json::from_str::<RetriggerRecord>(entry.note.as_deref()?).ok()
            })
            .find(|record| record.pr_number == pr && record.head_sha == head);
        if found.is_some() {
            return Ok(found);
        }
    }
    Ok(None)
}

/// Record the re-trigger on every owning task before the forge is asked, so
/// an interrupted request is never repeated for the same head.
fn record_retrigger<H: RuntimeHost + ?Sized>(
    host: &H,
    task_ids: &[String],
    record: &RetriggerRecord,
) -> Result<(), OrbitError> {
    if task_ids.is_empty() {
        return Err(OrbitError::Execution(format!(
            "pr_complete: pull request #{} has no owning task to record its check re-trigger",
            record.pr_number
        )));
    }
    let note =
        serde_json::to_string(record).map_err(|error| OrbitError::Execution(error.to_string()))?;
    for task in task_ids {
        host.apply_task_automation_update(
            task,
            TaskAutomationUpdate {
                append_history: vec![
                    TaskHistoryEntry::new(chrono::Utc::now(), "system", CHECKS_RETRIGGER_EVENT)
                        .with_note(Some(note.clone())),
                ],
                ..TaskAutomationUpdate::default()
            },
        )?;
    }
    Ok(())
}
