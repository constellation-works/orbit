//! Run ids are minted per workspace, so two workspaces submitting in the same
//! minute hold the same id. An invocation recorded under one workspace's run
//! never reaches the other's run listing or reliability counts.

#![allow(clippy::expect_used, clippy::unwrap_used, missing_docs)]

use chrono::{Duration, Utc};
use orbit_store::Store;
use orbit_store::compose::workspace_job_run_store;
use orbit_store::contracts::{InvocationInsertParams, InvocationQuery};
use orbit_types::telemetry::InvocationTrace;

/// A store where workspaces `ws_a` and `ws_b` each hold a run with the same
/// id. Retries once if the two submissions straddle a minute boundary.
fn colliding_runs() -> (Store, String) {
    for _ in 0..3 {
        let store = Store::open_in_memory().expect("memory store");
        let submit = |workspace: &str| {
            workspace_job_run_store(store.clone(), workspace)
                .insert_job_run("job", 1, Utc::now(), None, None)
                .expect("insert run")
                .run_id
        };
        let a = submit("ws_a");
        let b = submit("ws_b");
        if a == b {
            return (store, a);
        }
    }
    panic!("two workspaces submitting in one minute mint the same run id");
}

fn record(store: &Store, workspace_id: &str, run_id: &str, activity_id: &str) {
    store
        .insert_invocation_trace_record(
            workspace_id,
            &InvocationInsertParams {
                job_run_id: run_id.to_string(),
                activity_id: activity_id.to_string(),
                agent: "claude".to_string(),
                provider: None,
                model: None,
                task_ids: Vec::new(),
                trace: InvocationTrace::default(),
            },
        )
        .expect("record invocation");
}

#[test]
fn colliding_run_ids_keep_invocations_in_their_own_workspace() {
    let (store, run_id) = colliding_runs();
    record(&store, "ws_b", &run_id, "implement_one");
    let since = Utc::now() - Duration::hours(1);
    let until = Utc::now() + Duration::hours(1);

    let run_listing = |workspace: &str| {
        store
            .list_invocation_records(&InvocationQuery {
                workspace_id: Some(workspace.to_string()),
                job_run_id: Some(run_id.clone()),
                ..InvocationQuery::default()
            })
            .expect("list run invocations")
    };
    assert!(
        run_listing("ws_a").is_empty(),
        "ws_a's run listing read ws_b's invocation"
    );
    assert_eq!(run_listing("ws_b").len(), 1);

    assert!(
        store
            .count_invocations_by_activity("ws_a", since, until)
            .expect("count ws_a")
            .is_empty(),
        "ws_a's reliability counts included ws_b's invocation"
    );
    let b_counts = store
        .count_invocations_by_activity("ws_b", since, until)
        .expect("count ws_b");
    assert_eq!(b_counts.len(), 1);
    assert_eq!(b_counts[0].invocation_count, 1);

    let coverage = |workspace: &str| {
        store
            .count_invocation_job_runs(workspace, since, until, &["implement_one".to_string()])
            .expect("count run coverage")
    };
    assert_eq!(coverage("ws_a").total_job_runs, 0);
    assert_eq!(coverage("ws_b").matching_job_runs, 1);
}
