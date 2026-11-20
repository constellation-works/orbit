//! The `standing-drain` row (standing drain phase 1, ORB-25965): on the
//! owner host, the workspace's drain intent and how long work waited with no
//! drain admitting it over the last week.
//!
//! Read-only and informational: it reports, and nothing acts on it.

use orbit_core::{IDLE_WITH_WORK_DAYS, IdleWithWork, OrbitRuntime};

use super::{WorkspaceDoctorResult, WorkspaceDoctorStatus};

const CHECK: &str = "standing-drain";

pub(super) fn standing_drain_row(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    if let Some(owner) = runtime.coordination_write_owner() {
        return row(
            WorkspaceDoctorStatus::Skipped,
            format!(
                "this checkout is a replica of {owner}; the owner host reports drain idle time"
            ),
        );
    }
    let intent = match runtime.workspace_drain_intent() {
        Ok(None) => "off".to_string(),
        Ok(Some(record)) => {
            let at = record
                .updated_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            match record.intent {
                orbit_store::contracts::DrainIntentState::On => {
                    format!(
                        "on (started by {} from {} at {at})",
                        record.actor, record.source
                    )
                }
                orbit_store::contracts::DrainIntentState::Paused => {
                    format!("paused by {} from {} at {at}", record.actor, record.source)
                }
            }
        }
        Err(error) => format!("unreadable ({error})"),
    };
    match runtime.drain_idle_with_work(IDLE_WITH_WORK_DAYS) {
        Ok(idle) => row(
            WorkspaceDoctorStatus::Ok,
            format!("Drain intent: {intent}. {}", idle_sentence(&idle)),
        ),
        Err(error) => row(
            WorkspaceDoctorStatus::Warning,
            format!("Drain intent: {intent}. Cannot measure idle-with-work time: {error}"),
        ),
    }
}

/// The measured figure, then one `day hours` pair per UTC day, oldest first,
/// so a reader can take the daily series from the text.
fn idle_sentence(idle: &IdleWithWork) -> String {
    let daily = idle
        .days
        .iter()
        .map(|day| format!("{} {:.1} h", day.day, day.idle_hours))
        .collect::<Vec<_>>()
        .join(", ");
    let mut sentence = format!(
        "Idle with work waiting, last {} UTC days: {:.1} h with no admission window while \
         backlog or proposed work existed ({:.1} task-hours over {} tasks; drains admitted \
         for {:.1} h). Daily: {daily}.",
        idle.days.len(),
        idle.idle_hours,
        idle.idle_task_hours,
        idle.idle_tasks,
        idle.window_hours,
    );
    if idle.unreadable_histories > 0 {
        sentence.push_str(&format!(
            " {} task histories were unreadable and are not counted.",
            idle.unreadable_histories
        ));
    }
    sentence
}

fn row(status: WorkspaceDoctorStatus, message: String) -> WorkspaceDoctorResult {
    WorkspaceDoctorResult {
        duration_ms: 0,
        check_name: CHECK.into(),
        status,
        message,
        remediation: None,
    }
}
