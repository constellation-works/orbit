//! Orchestrator accounting isolates workspaces sharing the host invocation store.

use chrono::{Duration, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, OrchestratorMetricsBucketKind};
use orbit_store::contracts::{InvocationInsertParams, InvocationQuery};
use orbit_types::telemetry::{InvocationTrace, TokenUsage};
use tempfile::TempDir;

use super::dispatch_admission::isolated;

fn record(runtime: &OrbitRuntime, task_ids: &[String], scale: u64) {
    runtime
        .insert_invocation_trace_record(&InvocationInsertParams {
            job_run_id: "fixture-run".to_string(),
            activity_id: "fixture".to_string(),
            agent: "codex".to_string(),
            model: Some("gpt-6.1-sol".to_string()),
            task_ids: task_ids.to_vec(),
            trace: InvocationTrace {
                usage: TokenUsage {
                    input: 100 * scale,
                    cache_read: 10 * scale,
                    cache_create: 20 * scale,
                    cache_create_1h: 5 * scale,
                    output: 40 * scale,
                },
                ..Default::default()
            },
        })
        .expect("record invocation under the runtime workspace");
}

#[test]
fn orchestrator_accounting_excludes_other_workspaces_and_keeps_local_missing_tasks() {
    if !isolated(
        "invocation_metrics::orchestrator_accounting_excludes_other_workspaces_and_keeps_local_missing_tasks",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();
    let global = root.path().join("global");
    let open = |name: &str| {
        let workspace = root.path().join(name).join(".orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let git = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(workspace.parent().unwrap())
            .output()
            .expect("initialize disposable fixture checkout");
        assert!(git.status.success(), "fixture git init: {git:?}");
        OrbitRuntime::from_roots(&global, &workspace).expect("open workspace runtime")
    };
    let a = open("a");
    let b = open("b");
    assert_ne!(a.workspace_id().unwrap(), b.workspace_id().unwrap());
    let task = |runtime: &OrbitRuntime| {
        runtime
            .add_task(TaskAddParams {
                title: "Accounting fixture".to_string(),
                ..Default::default()
            })
            .expect("create task in fixture workspace")
            .id
    };
    let a_task = task(&a);
    let b_task = task(&b);
    let since = Utc::now() - Duration::hours(1);
    record(&a, std::slice::from_ref(&a_task), 1);
    // B's task exists on the host but is missing from A's task population.
    record(&a, std::slice::from_ref(&b_task), 2);
    let before = a
        .orchestrator_invocation_metrics(None, None)
        .expect("metrics before foreign invocations");
    assert_eq!(before.buckets.len(), 2);
    let missing = before
        .buckets
        .iter()
        .find(|bucket| bucket.kind == OrchestratorMetricsBucketKind::Missing)
        .expect("local invocation linked to a missing task remains visible");
    assert_eq!(missing.invocation_count, 1);
    assert_eq!(missing.normalized_tokens.normalized_token_total, 280);
    let tokens = &before.normalized_tokens;
    assert_eq!(tokens.invocation_count, 2);
    assert_eq!(tokens.covered_invocation_count, 2);
    assert_eq!(tokens.linked_task_count, 2);
    assert_eq!(tokens.uncached_input_tokens, 195);
    assert_eq!(tokens.cache_read_tokens, 30);
    assert_eq!(tokens.cache_create_tokens, 60);
    assert_eq!(tokens.cache_create_1h_tokens, 15);
    assert_eq!(tokens.output_tokens, 120);
    assert_eq!(tokens.normalized_token_total, 420);

    // Foreign rows would otherwise contaminate Missing and Unattributed,
    // even when they have no links or name a task that A can resolve.
    record(&b, std::slice::from_ref(&b_task), 10);
    record(&b, &[], 20);
    record(&b, std::slice::from_ref(&a_task), 30);
    assert_eq!(
        a.invocation_records(InvocationQuery {
            limit: 100,
            ..Default::default()
        })
        .expect("shared host invocation listing")
        .len(),
        5,
        "both workspaces write into the same invocation store"
    );
    for lower in [None, Some(since)] {
        let after = a
            .orchestrator_invocation_metrics(lower, None)
            .expect("workspace accounting with and without a lower cutoff");
        assert_eq!(
            after.buckets, before.buckets,
            "foreign facts reach no bucket"
        );
        assert_eq!(
            after.normalized_tokens, before.normalized_tokens,
            "foreign facts contribute no normalized token totals"
        );
    }
}
