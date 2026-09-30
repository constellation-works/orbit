use std::collections::BTreeMap;

use chrono::Utc;
use orbit_types::workflow::{
    ChildDispatch, DrainAdmissionPass, DrainWaitingTask, JobRun, JobRunState, PipelineState,
};
use serde_json::{Value, json};

use super::super::drain_summary::{DRAIN_JOB, summarize_drain_leaves};

fn run(run_id: &str, job_id: &str, state: JobRunState, input: Option<Value>) -> JobRun {
    let now = Utc::now();
    JobRun {
        executed_on: None,
        run_id: run_id.to_string(),
        job_id: job_id.to_string(),
        attempt: 1,
        state,
        scheduled_at: now,
        started_at: None,
        finished_at: None,
        duration_ms: None,
        created_at: now,
        pid: None,
        pid_start_time: None,
        input,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    }
}

fn drain_state(children: &[&str], last_pass: Option<DrainAdmissionPass>) -> PipelineState {
    let mut state = PipelineState::new("jrun-drain".to_string(), DRAIN_JOB.to_string(), json!({}));
    for child in children {
        state.record_child_dispatch(ChildDispatch::submitted(
            (*child).to_string(),
            "task_auto_pipeline".to_string(),
            "invoke_detached".to_string(),
            false,
            false,
            Utc::now(),
        ));
    }
    state.drain_last_pass = last_pass;
    state
}

fn leaves(runs: Vec<JobRun>) -> impl FnMut(&str) -> Option<JobRun> {
    let by_id: BTreeMap<String, JobRun> = runs
        .into_iter()
        .map(|run| (run.run_id.clone(), run))
        .collect();
    move |id| by_id.get(id).cloned()
}

#[test]
fn a_successful_drain_still_reports_its_failed_leaf_and_the_task_left_waiting() {
    let drain = run("jrun-drain", DRAIN_JOB, JobRunState::Success, None);
    let state = drain_state(
        &["jrun-ok", "jrun-bad", "jrun-live"],
        Some(DrainAdmissionPass {
            recorded_at: Utc::now(),
            queued: 1,
            deferred: vec![DrainWaitingTask {
                task_id: "ORB-9".to_string(),
                reason: None,
                blocked_by: vec!["ORB-2".to_string()],
            }],
            excluded: vec![DrainWaitingTask {
                task_id: "ORB-7".to_string(),
                reason: Some("context_lock_conflict".to_string()),
                blocked_by: vec!["ORB-2".to_string()],
            }],
            excluded_total: 3,
        }),
    );
    let summary = summarize_drain_leaves(
        &drain,
        Some(&state),
        leaves(vec![
            run(
                "jrun-ok",
                "task_auto_pipeline",
                JobRunState::Success,
                Some(json!({ "task_ids": ["ORB-1"] })),
            ),
            run(
                "jrun-bad",
                "task_auto_pipeline",
                JobRunState::Failed,
                Some(json!({ "task_ids": ["ORB-2"] })),
            ),
            run(
                "jrun-live",
                "task_auto_pipeline",
                JobRunState::Running,
                Some(json!({ "task_ids": ["ORB-3"] })),
            ),
        ]),
    )
    .expect("a drain summarizes its leaves");

    assert_eq!(
        (
            summary.admitted,
            summary.succeeded,
            summary.failed,
            summary.running
        ),
        (3, 1, 1, 1)
    );
    assert!(summary.has_failed_leaves());
    assert!(summary.has_starved_tasks());
    assert_eq!(summary.failed_leaves[0].run_id, "jrun-bad");
    assert_eq!(summary.failed_leaves[0].task_ids, vec!["ORB-2".to_string()]);
    assert_eq!(summary.waiting.queued, Some(1));
    assert_eq!(summary.waiting.excluded_total, 3);
    assert_eq!(
        summary.waiting.deferred[0].blocked_by,
        vec!["ORB-2".to_string()]
    );

    let json = summary.to_json();
    assert_eq!(json["failed"], 1);
    assert_eq!(json["has_failed_leaves"], true);
    assert_eq!(json["waiting"]["excluded"][0]["task_id"], "ORB-7");

    let mut summary = summary;
    // The caller reads the run tree and names the run that actually retries the
    // failed work; a leaf whose failure came from a child resumes that child.
    assert_eq!(summary.failed_leaves[0].resume_run_id, None);
    summary.failed_leaves[0].resume_run_id = Some("jrun-bad-c3".to_string());
    assert_eq!(
        summary.to_json()["failed_leaves"][0]["resume_run_id"],
        "jrun-bad-c3"
    );

    let text = summary.lines(JobRunState::Success).join("\n");
    assert!(
        text.contains("WARNING:"),
        "failed leaves are flagged: {text}"
    );
    assert!(
        text.contains("orbit job resume jrun-bad-c3"),
        "the failed leaf's retry command is named: {text}"
    );
    assert!(
        text.contains("Still waiting:") && text.contains("blocked-by=ORB-2"),
        "starved work names its holder: {text}"
    );
}

#[test]
fn a_clean_drain_reports_counts_without_warnings() {
    let drain = run("jrun-drain", DRAIN_JOB, JobRunState::Success, None);
    let state = drain_state(
        &["jrun-ok"],
        Some(DrainAdmissionPass {
            recorded_at: Utc::now(),
            queued: 0,
            deferred: Vec::new(),
            excluded: Vec::new(),
            excluded_total: 0,
        }),
    );
    let summary = summarize_drain_leaves(
        &drain,
        Some(&state),
        leaves(vec![run(
            "jrun-ok",
            "task_auto_pipeline",
            JobRunState::Success,
            None,
        )]),
    )
    .expect("summary");

    assert_eq!(summary.waiting.queued, Some(0));
    assert!(!summary.has_failed_leaves());
    assert!(!summary.has_starved_tasks());
    let text = summary.lines(JobRunState::Success).join("\n");
    assert!(text.contains("admitted=1 succeeded=1 failed=0"), "{text}");
    assert!(!text.contains("WARNING:"), "{text}");
    assert!(!text.contains("Still waiting:"), "{text}");
}

#[test]
fn a_run_that_is_not_a_drain_has_no_leaf_summary() {
    let other = run(
        "jrun-other",
        "task_auto_pipeline",
        JobRunState::Failed,
        None,
    );
    let state = drain_state(&["jrun-child"], None);
    assert!(summarize_drain_leaves(&other, Some(&state), |_| None).is_none());
}

#[test]
fn an_unreadable_leaf_is_counted_rather_than_failing_the_view() {
    let drain = run("jrun-drain", DRAIN_JOB, JobRunState::Success, None);
    let state = drain_state(&["jrun-gone"], None);
    let summary = summarize_drain_leaves(&drain, Some(&state), |_| None).expect("summary");
    assert_eq!((summary.admitted, summary.unreadable), (1, 1));
    assert!(summary.lines(JobRunState::Success)[0].contains("unreadable=1"));
}
