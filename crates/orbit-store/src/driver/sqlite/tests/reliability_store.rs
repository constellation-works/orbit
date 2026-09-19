//! [ORB-10588] Count-only reliability reads.
//!
//! The properties worth pinning here are the ones a wrong query would get
//! quietly wrong: workspace scoping (including for `invocations`, which has no
//! `workspace_id` of its own), half-open window boundaries, and the fact that
//! distinct-run coverage is computed in one pass rather than summed.

use chrono::{DateTime, Duration, TimeZone, Utc};
use orbit_types::telemetry::InvocationTrace;
use orbit_types::workflow::{JobRun, JobRunState};

use crate::Store;
use crate::contracts::InvocationInsertParams;

const WS: &str = "ws-primary";
const OTHER_WS: &str = "ws-other";

fn at(hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 1, hour, 0, 0)
        .single()
        .expect("valid timestamp")
}

fn insert_run(
    store: &Store,
    workspace: &str,
    run_id: &str,
    job_id: &str,
    state: JobRunState,
    created: DateTime<Utc>,
) {
    let run = JobRun {
        executed_on: None,
        run_id: run_id.to_string(),
        job_id: job_id.to_string(),
        attempt: 1,
        state,
        scheduled_at: created,
        started_at: Some(created),
        finished_at: None,
        duration_ms: None,
        created_at: created,
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    };
    store
        .upsert_job_run_for_workspace(workspace, &run, None)
        .expect("insert job run");
}

fn insert_invocation(store: &Store, run_id: &str, activity_id: &str) {
    store
        .insert_invocation_trace_record(&InvocationInsertParams {
            job_run_id: run_id.to_string(),
            activity_id: activity_id.to_string(),
            agent: "agent".to_string(),
            model: Some("model".to_string()),
            task_ids: Vec::new(),
            trace: InvocationTrace::default(),
        })
        .expect("insert invocation");
}

fn window() -> (DateTime<Utc>, DateTime<Utc>) {
    (at(0), at(12))
}

/// Window for the invocation-backed reads.
///
/// `insert_invocation_trace_record` stamps `ts` itself at insert time, so a
/// fixture cannot place an invocation at a chosen instant the way it can place
/// a job run. These tests therefore bracket wall-clock now.
fn invocation_window() -> (DateTime<Utc>, DateTime<Utc>) {
    (
        Utc::now() - Duration::hours(1),
        Utc::now() + Duration::hours(1),
    )
}

#[test]
fn job_run_facts_are_scoped_to_one_workspace() {
    let store = Store::open_in_memory().expect("open store");
    insert_run(&store, WS, "jrun-1", "alpha", JobRunState::Success, at(1));
    insert_run(
        &store,
        OTHER_WS,
        "jrun-2",
        "alpha",
        JobRunState::Failed,
        at(1),
    );

    let (since, until) = window();
    let result = store
        .list_job_run_outcome_facts(WS, since, until, 100)
        .expect("list facts");

    assert_eq!(result.facts.len(), 1);
    assert_eq!(result.facts[0].job_id, "alpha");
    assert_eq!(result.facts[0].state, "success");
    assert!(!result.truncated);
}

#[test]
fn the_window_is_half_open_on_both_ends() {
    let store = Store::open_in_memory().expect("open store");
    insert_run(
        &store,
        WS,
        "before",
        "j",
        JobRunState::Success,
        at(0) - Duration::seconds(1),
    );
    insert_run(&store, WS, "at-since", "j", JobRunState::Success, at(0));
    insert_run(&store, WS, "at-until", "j", JobRunState::Success, at(12));

    let (since, until) = window();
    let result = store
        .list_job_run_outcome_facts(WS, since, until, 100)
        .expect("list facts");

    // `since` is inclusive, `until` exclusive — so adjacent windows tile
    // without double-counting a run on the boundary.
    assert_eq!(result.facts.len(), 1);
}

#[test]
fn the_row_cap_reports_truncation_instead_of_silently_dropping_runs() {
    let store = Store::open_in_memory().expect("open store");
    for index in 0..5 {
        insert_run(
            &store,
            WS,
            &format!("jrun-{index}"),
            "j",
            JobRunState::Success,
            at(1) + Duration::seconds(index),
        );
    }

    let (since, until) = window();
    let capped = store
        .list_job_run_outcome_facts(WS, since, until, 3)
        .expect("list facts");
    assert_eq!(capped.facts.len(), 3);
    assert!(capped.truncated, "a bound that binds must be reported");

    let exact = store
        .list_job_run_outcome_facts(WS, since, until, 5)
        .expect("list facts");
    assert_eq!(exact.facts.len(), 5);
    assert!(
        !exact.truncated,
        "a page that ends exactly on the cap is not truncated"
    );
}

#[test]
fn invocations_are_scoped_by_the_workspace_of_their_owning_run() {
    // `invocations` carries no workspace_id, so this is the only thing keeping
    // one workspace's recovery rate out of another's.
    let store = Store::open_in_memory().expect("open store");
    insert_run(&store, WS, "jrun-mine", "j", JobRunState::Success, at(1));
    insert_run(
        &store,
        OTHER_WS,
        "jrun-theirs",
        "j",
        JobRunState::Success,
        at(1),
    );
    insert_invocation(&store, "jrun-mine", "implement");
    insert_invocation(&store, "jrun-theirs", "implement");

    let (since, until) = invocation_window();
    let counts = store
        .count_invocations_by_activity(WS, since, until)
        .expect("count invocations");

    assert_eq!(counts.len(), 1);
    assert_eq!(counts[0].activity_id, "implement");
    assert_eq!(counts[0].invocation_count, 1);
    assert_eq!(counts[0].job_run_count, 1);
}

#[test]
fn an_invocation_with_no_matching_run_row_is_excluded_rather_than_guessed() {
    let store = Store::open_in_memory().expect("open store");
    insert_invocation(&store, "jrun-orphan", "implement");

    let (since, until) = invocation_window();
    let counts = store
        .count_invocations_by_activity(WS, since, until)
        .expect("count invocations");
    assert!(counts.is_empty());
}

#[test]
fn activity_counts_separate_invocations_from_runs_touched() {
    let store = Store::open_in_memory().expect("open store");
    insert_run(&store, WS, "jrun-a", "j", JobRunState::Success, at(1));
    insert_run(&store, WS, "jrun-b", "j", JobRunState::Success, at(1));
    for _ in 0..3 {
        insert_invocation(&store, "jrun-a", "implement");
    }
    insert_invocation(&store, "jrun-b", "implement");

    let (since, until) = invocation_window();
    let counts = store
        .count_invocations_by_activity(WS, since, until)
        .expect("count invocations");

    assert_eq!(counts[0].invocation_count, 4);
    assert_eq!(counts[0].job_run_count, 2, "distinct runs, not invocations");
}

#[test]
fn run_coverage_counts_distinct_runs_across_all_selected_activities() {
    // Summing per-activity distinct-run counts would give 3 here (2 + 1) even
    // though only 2 distinct runs used recovery. This is why coverage is a
    // dedicated single-pass query.
    let store = Store::open_in_memory().expect("open store");
    for run_id in ["jrun-a", "jrun-b", "jrun-c"] {
        insert_run(&store, WS, run_id, "j", JobRunState::Success, at(1));
        insert_invocation(&store, run_id, "implement");
    }
    insert_invocation(&store, "jrun-a", "rescue_one");
    insert_invocation(&store, "jrun-b", "rescue_one");
    insert_invocation(&store, "jrun-a", "rescue_two");

    let (since, until) = invocation_window();
    let coverage = store
        .count_invocation_job_runs(
            WS,
            since,
            until,
            &["rescue_one".to_string(), "rescue_two".to_string()],
        )
        .expect("count coverage");

    assert_eq!(coverage.total_job_runs, 3);
    assert_eq!(coverage.matching_job_runs, 2);
}

#[test]
fn run_coverage_with_no_selected_activities_still_reports_a_denominator() {
    let store = Store::open_in_memory().expect("open store");
    insert_run(&store, WS, "jrun-a", "j", JobRunState::Success, at(1));
    insert_invocation(&store, "jrun-a", "implement");

    let (since, until) = invocation_window();
    let coverage = store
        .count_invocation_job_runs(WS, since, until, &[])
        .expect("count coverage");

    assert_eq!(coverage.total_job_runs, 1);
    assert_eq!(coverage.matching_job_runs, 0);
}
