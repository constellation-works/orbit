//! Live agent processes on this workspace, grouped for the drain card.
//!
//! A process counts when its job run is `running` or `retrying` and its
//! provider child is still alive. The group's name is the enclosing
//! `activity.started` event's `activity_name`. A pipeline step records its
//! step id there; a recovery dispatch records the recovery activity's name.
//! Pending, held and finished runs do not count, nor does a child that has
//! exited.

use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::workflow::JobRunState;
use serde_json::{Value, json};

use crate::OrbitRuntime;

/// `activity.started` names that are task pilots.
///
/// `pilot`, `repair_pilot` and `empty_pilot` are the `task_pilot` workers
/// (`workflow.purpose_crews.task_pilot`). `triager` is the
/// `triage_escalation` worker; its only subject is `pilot_warning`
/// (`workflow.purpose_crews.pilot_warning_triage`). The catalog names are
/// included for a dispatch that records the activity rather than the step.
const PILOT_ACTIVITIES: &[&str] = &[
    "pilot",
    "repair_pilot",
    "empty_pilot",
    "task_pilot",
    "triager",
    "triage_escalation",
];

/// `activity.started` names that fix or recover work.
///
/// `step_failure_recovery` is purpose `failure_recovery`. An `--auto-fix`
/// repair keeps that activity name and only switches the crew key to
/// `workflow.purpose_crews.auto_fix`, so it lands here too. A failed review
/// step recovered the same way is recovery, not another reviewer.
/// `pr_conflict_recovery` is the pull-request conflict leaf.
/// `final_recovery` is the job's terminal recovery activity
/// (`workflow.final_recovery_crews`). `recover` is the
/// `blocked_task_recovery_pipeline` step that targets `final_recovery`.
const RECOVERY_ACTIVITIES: &[&str] = &[
    "step_failure_recovery",
    "pr_conflict_recovery",
    "final_recovery",
    "recover",
];

/// `activity.started` names that review.
///
/// `review` is `agent_review_repair` (purpose `review`) and
/// `review_reconciliation_review` (purpose `pre_landing_review`, which draws
/// the review pool). `landing_review`, `re_review` and `re_review_2` are the
/// other `agent_review_repair` steps. The catalog names cover a dispatch that
/// records the activity rather than the step.
const REVIEWER_ACTIVITIES: &[&str] = &[
    "review",
    "landing_review",
    "re_review",
    "re_review_2",
    "agent_review_repair",
    "review_reconciliation_review",
];

#[derive(Default)]
struct LiveAgentCounts {
    pilots: u64,
    recovery: u64,
    reviewers: u64,
}

/// Three integers for the drain card, or nothing when the runs cannot be read.
///
/// A zero result is still returned: the card shows the figures at zero. One
/// run whose audit cannot be read adds nothing and does not hide the others.
/// `visual_review` is not a routed purpose. An ordinary `agent_implement`
/// (`implement_one`), including a baseline-red repair or a sweep, is not one
/// of these three: the live process carries the step id, not the task's tags.
pub(super) fn observe_live_agents(runtime: &OrbitRuntime) -> Option<Value> {
    match live_agent_counts(runtime) {
        Ok(counts) => Some(json!({
            "pilots": counts.pilots,
            "recovery": counts.recovery,
            "reviewers": counts.reviewers,
        })),
        Err(error) => {
            tracing::warn!("readiness shows no live agent counts: {error}");
            None
        }
    }
}

fn live_agent_counts(runtime: &OrbitRuntime) -> Result<LiveAgentCounts, OrbitError> {
    let runs = runtime
        .stores()
        .jobs()
        .list_job_runs_filtered(&JobRunQuery {
            states: vec![JobRunState::Running, JobRunState::Retrying],
            include_steps: false,
            input_keys: Some(&[]),
            ..JobRunQuery::default()
        })?;
    let mut counts = LiveAgentCounts::default();
    if runs.is_empty() {
        return Ok(counts);
    }
    let run_ids: Vec<String> = runs.into_iter().map(|run| run.run_id).collect();
    let workspace_id = runtime.workspace_id()?;
    let present = runtime
        .v2_audit_store()?
        .list_v2_audit_run_ids_with_events(&workspace_id, &run_ids, Some("v2_envelope"))?;
    for run_id in run_ids {
        if !present.contains(&run_id) {
            continue;
        }
        let names = match runtime.live_agent_activity_names(&run_id) {
            Ok(names) => names,
            Err(error) => {
                tracing::warn!(
                    run_id = %run_id,
                    "readiness skips a live agent run: {error}"
                );
                continue;
            }
        };
        for name in names {
            match classify_live_agent(&name) {
                Some(LiveAgentKind::Pilots) => counts.pilots = counts.pilots.saturating_add(1),
                Some(LiveAgentKind::Recovery) => {
                    counts.recovery = counts.recovery.saturating_add(1);
                }
                Some(LiveAgentKind::Reviewers) => {
                    counts.reviewers = counts.reviewers.saturating_add(1);
                }
                None => {}
            }
        }
    }
    Ok(counts)
}

enum LiveAgentKind {
    Pilots,
    Recovery,
    Reviewers,
}

fn classify_live_agent(activity_name: &str) -> Option<LiveAgentKind> {
    if PILOT_ACTIVITIES.contains(&activity_name) {
        Some(LiveAgentKind::Pilots)
    } else if RECOVERY_ACTIVITIES.contains(&activity_name) {
        Some(LiveAgentKind::Recovery)
    } else if REVIEWER_ACTIVITIES.contains(&activity_name) {
        Some(LiveAgentKind::Reviewers)
    } else {
        None
    }
}
