//! Terminal rendering of drain leaves and admission passes.
use super::format::{WaitingTaskFormatter, summarize_error_message, waiting_reason};
use orbit_core::app::run::{DrainLeafSummary, WaitingBacklog, WaitingTask, waiting_backlog};
use orbit_types::workflow::{
    DrainAdmissionPass, DrainApprovalReport, JobRunState, ResourceThrottle,
};
use serde_json::json;
/// Consecutive idle owner answers after which a pull drain says why it
/// claims nothing, rather than leaving the per-task lines to be added up.
const IDLE_SUMMARY_PASSES: u32 = 3;

/// The `Leaves:` line, then a warning per failed or starved outcome.
pub(super) fn leaf_lines(
    summary: &DrainLeafSummary,
    drain_state: JobRunState,
    formatter: &WaitingTaskFormatter,
) -> Vec<String> {
    use crate::output::common::color::bold;
    let mut lines = vec![format!(
        "{} admitted={} succeeded={} failed={} running={} cancelled={}{}{}",
        bold("Leaves:"),
        summary.admitted,
        summary.succeeded,
        summary.failed,
        summary.running,
        summary.cancelled,
        if summary.unreadable > 0 {
            format!(" unreadable={}", summary.unreadable)
        } else {
            String::new()
        },
        if summary.held_in_review > 0 {
            format!(" held_in_review={}", summary.held_in_review)
        } else {
            String::new()
        },
    )];
    if summary.held_in_review > 0 {
        lines.push(format!(
            "{} {}",
            bold("Held in review:"),
            summary.held_in_review
        ));
        let breakdown = summary
            .held_in_review_by_reason
            .iter()
            .map(|(reason, ids)| format!("{reason}={} {}", ids.len(), ids.join(",")))
            .collect::<Vec<_>>()
            .join("; ");
        if !breakdown.is_empty() {
            lines.push(format!("  {breakdown}"));
        }
        for leaf in &summary.held_leaves {
            for task in &leaf.tasks {
                let pr = task
                    .pr_number
                    .as_deref()
                    .map(|number| format!(" pr={number}"))
                    .unwrap_or_default();
                let matched = task
                    .matched
                    .as_deref()
                    .map(|value| format!(" matched={value}"))
                    .unwrap_or_default();
                lines.push(format!(
                    "  {} task={} {} {} run={}{pr}{matched}",
                    leaf.run_id, task.task_id, task.hold, task.reason, task.delivery_run_id
                ));
            }
        }
    }
    if let Some(capacity) = &summary.capacity {
        lines.push(format!(
            "{} occupied={} inherited={} limit={} (last admission pass {})",
            bold("Capacity:"),
            capacity.active_leaf_runs,
            capacity.inherited_leaf_runs,
            capacity.max_active_leaf_runs,
            summary
                .waiting
                .recorded_at
                .map(|at| at.to_rfc3339())
                .unwrap_or_default(),
        ));
    }
    if summary.has_failed_leaves() {
        let note = if drain_state == JobRunState::Success {
            " (the drain's own `success` only means the sequencer finished; it is not a leaf outcome)"
        } else {
            ""
        };
        lines.push(format!(
            "{} {} of {} leaves failed{note}",
            bold("WARNING:"),
            summary.failed_leaves.len(),
            summary.admitted,
        ));
        for leaf in &summary.failed_leaves {
            // One run id per failure: the root-cause run the retry would
            // resume, not the drain's wrapper leaf. `resume_hint` on a
            // resumable run's own header is a different line and stays;
            // a finished drain never prints it.
            let run_id = leaf.resume_run_id.as_deref().unwrap_or(&leaf.run_id);
            let tasks = if leaf.task_ids.is_empty() {
                "-".to_string()
            } else {
                leaf.task_ids
                    .iter()
                    .map(|id| formatter.task_with_status(id))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            lines.push(format!(
                "  Failed leaf {run_id} task={tasks} state={} error={}",
                leaf.state,
                summarize_error_message(leaf.error.as_deref()),
            ));
            // Keep the retry only while a named task is still blocked, or
            // its status cannot be read. A task readmitted or completed
            // has left `blocked`, and the historical leaf must not advise
            // resuming it.
            let retry = leaf.task_ids.is_empty()
                || leaf
                    .task_ids
                    .iter()
                    .any(|id| formatter.retry_still_applies(id));
            if retry {
                lines.push(match &leaf.resume_run_id {
                    Some(resume) => format!(
                        "    retry: `orbit job resume {resume}` (or move the task back to backlog)"
                    ),
                    None => "    retry: move the task back to backlog".to_string(),
                });
            }
        }
    }
    if let Some(throttle) = summary.waiting.resource_throttle.as_ref() {
        lines.push(throttle_line(throttle, summary.waiting.recorded_at));
    }
    if let Some(approvals) = &summary.approvals {
        lines.extend(approval_lines(approvals, formatter));
    }
    lines.extend(waiting_lines(&summary.waiting, formatter));
    lines
}
/// The `Still waiting:` block, and for a pull drain that has found nothing
/// claimable here several passes running, the `idle:` line that says why.
fn waiting_lines(waiting: &WaitingBacklog, formatter: &WaitingTaskFormatter) -> Vec<String> {
    use crate::output::common::color::bold;
    if !waiting.has_starved_tasks() {
        return Vec::new();
    }
    // A pull drain's lists are the owner's last answer. A local drain
    // dates them only when it deferred a pass on a busy lock, which keeps
    // the last completed pass's lists.
    let answered = waiting
        .waiting_recorded_at
        .map(|answered| {
            let at = answered.format("%Y-%m-%d %H:%M:%SZ");
            if waiting.deferred_in_queued {
                format!(" (as of the last completed pass at {at})")
            } else {
                format!(" (the owner answered {at})")
            }
        })
        .unwrap_or_default();
    let mut lines = vec![format!(
        "{} {} admissible, {} deferred and {} excluded backlog task(s) were never started at the last pass{answered}",
        bold("Still waiting:"),
        waiting.queued_for_header(),
        waiting.deferred_count(),
        waiting.excluded_total,
    )];
    for task in &waiting.deferred {
        lines.push(format!(
            "  {}",
            waiting_line(task, "context_lock_conflict", formatter)
        ));
    }
    let deferred_listed = waiting.deferred.len() as u64;
    if waiting.deferred_count() > deferred_listed {
        lines.push(format!(
            "  ... and {} more deferred",
            waiting.deferred_count() - deferred_listed
        ));
    }
    for task in &waiting.excluded {
        lines.push(format!("  {}", waiting_line(task, "excluded", formatter)));
    }
    let listed = waiting.excluded.len() as u64;
    if waiting.excluded_total > listed {
        lines.push(format!(
            "  ... and {} more excluded",
            waiting.excluded_total - listed
        ));
    }
    if let Some(line) = idle_line(waiting) {
        lines.push(line);
    }
    lines
}

fn idle_line(waiting: &WaitingBacklog) -> Option<String> {
    let kept_off: u64 = waiting.by_reason.values().sum();
    if waiting.idle_passes < IDLE_SUMMARY_PASSES || kept_off == 0 {
        return None;
    }
    let mut by_count = waiting.by_reason.iter().collect::<Vec<_>>();
    by_count.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let causes = by_count
        .into_iter()
        .map(|(reason, count)| format!("{count} {}", kept_off_cause(reason)))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "  idle: {kept_off} backlog task(s) kept off this host for {} consecutive passes ({causes})",
        waiting.idle_passes,
    ))
}

/// What a pull drain's reason code says about the tasks it counts.
fn kept_off_cause(reason: &str) -> &str {
    match reason {
        "context_lock_conflict" => "footprint holds",
        "dependency_not_done" | "unmet_dependency" => "unmet dependencies",
        "host_os_mismatch" => "for another OS",
        "host_labels_missing" => "needing a host with other capability labels",
        "browser_kit_missing" => "needing a host with the browser kit",
        "native_os_required" => "needing native evidence from another OS",
        "unverified_review_finding" => "review findings awaiting verification",
        "crew_unavailable" => "needing a crew this host cannot run",
        "owner_hold" => "held on the owner",
        "scheduled" => "scheduled for later",
        "invalid_candidate" => "invalid",
        other => other,
    }
}

/// The `Approved:` line for an `--approve-proposed` drain, then each held
/// proposed task with the reason it stayed proposed.
fn approval_lines(report: &DrainApprovalReport, formatter: &WaitingTaskFormatter) -> Vec<String> {
    let closed = if report.closed_total > 0 {
        format!(
            "; {} closed as already fixed ({})",
            report.closed_total,
            report.closed.join(", ")
        )
    } else {
        String::new()
    };
    let mut lines = vec![format!(
        "{} {} proposed task(s) moved to backlog{closed}; {} held{}",
        crate::output::common::color::bold("Approved:"),
        report.approved_total,
        report.held_total,
        if report.held_by_reason.is_empty() {
            String::new()
        } else {
            format!(
                " ({})",
                report
                    .held_by_reason
                    .iter()
                    .map(|(reason, count)| format!("{reason}={count}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
    )];
    for held in &report.held {
        let task = WaitingTask {
            task_id: held.task_id.clone(),
            reason: held.reason.clone(),
            blocked_by: held.blocked_by.clone(),
            detail: held.detail.clone(),
        };
        lines.push(format!("  {}", waiting_line(&task, "held", formatter)));
        for finding in &held.findings {
            lines.push(format!("    {}: {}", finding.kind, finding.text));
        }
    }
    let listed = report.held.len() as u64;
    if report.held_total > listed {
        lines.push(format!(
            "  ... and {} more held",
            report.held_total - listed
        ));
    }
    lines
}

/// The `Throttled:` line for a drain whose last pass host pressure held.
fn throttle_line(
    throttle: &ResourceThrottle,
    recorded_at: Option<chrono::DateTime<chrono::Utc>>,
) -> String {
    let at = recorded_at
        .map(|at| format!(" (last pass {})", at.format("%Y-%m-%d %H:%M:%SZ")))
        .unwrap_or_default();
    format!(
        "{} {}{at}",
        crate::output::common::color::bold("Throttled:"),
        throttle.hold_reason()
    )
}

/// The `Throttled:` line for a pull drain, which has no leaf summary of its
/// own: its owner orders the backlog [ORB-13901].
pub(super) fn pass_throttle_line(pass: &DrainAdmissionPass) -> Option<String> {
    pass.resource_throttle
        .as_ref()
        .map(|throttle| throttle_line(throttle, Some(pass.recorded_at)))
}

/// The `Requests held:` line for a pull drain whose last pass requested no
/// claim although its window was open, naming why: the breaker, a refused
/// settlement, the owner's probe or no runnable crew [ORB-15637].
pub(super) fn pass_refusal_line(pass: &DrainAdmissionPass) -> Option<String> {
    pass.admission_refusal.as_ref().map(|refusal| {
        format!(
            "{} {refusal} (last pass {})",
            crate::output::common::color::bold("Requests held:"),
            pass.recorded_at.format("%Y-%m-%d %H:%M:%SZ")
        )
    })
}

/// The `Still waiting:` lines for a pull drain, which has no leaf summary of
/// its own: they come from the owner's last answer, as the drain recorded it
/// [ORB-14475].
pub(super) fn pass_waiting_lines(
    pass: &DrainAdmissionPass,
    formatter: &WaitingTaskFormatter,
) -> Vec<String> {
    waiting_lines(&waiting_backlog(pass, false), formatter)
}

fn waiting_line(
    task: &WaitingTask,
    default_reason: &str,
    formatter: &WaitingTaskFormatter,
) -> String {
    let reason = waiting_reason(task.reason.as_deref().unwrap_or(default_reason));
    let mut line = format!("Task {}: {}", task.task_id, reason);
    line.push_str(&formatter.blockers(&json!({"reason": reason, "blocked_by": task.blocked_by})));
    // A host-OS wait names the host it waits for, a scheduled task its time,
    // a browser-kit wait the install command, and a native-OS requirement the tag to add. A local-route review hold names the
    // remedy. A verified-no-diff hold names the pilot's evidence and the
    // commits it cites, and an unverified review finding the verification
    // job to run. Other details (long repair instructions) stay in `--json`.
    if matches!(
        task.reason.as_deref(),
        Some(
            "host_os_mismatch"
                | "host_labels_missing"
                | "browser_kit_missing"
                | "native_os_required"
                | "local_route_before_pr"
                | "local_route_before_landing"
                | "crew_unavailable"
                | "owner_hold"
                | "scheduled"
                | "invalid_candidate"
                | "pilot_verified_no_diff"
                | "unverified_review_finding"
        )
    ) && let Some(detail) = &task.detail
    {
        match formatter.host_fleet(&task.task_id) {
            Some(fleet) => line.push_str(&format!(" ({detail}; {fleet})")),
            None => line.push_str(&format!(" ({detail})")),
        }
    }
    line
}
