//! Cross-pipeline tests for the shared duplicate-task assessment contract.

use std::cell::Cell;

use orbit_common::OrbitError;
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::ci_failure_tasks::file_ci_failure_tasks_with_lookup;
use crate::adapter::engine_host::v2_host::dependabot_alert_tasks::file_dependabot_alert_tasks_with_lookup;
use crate::adapter::engine_host::v2_host::duplicate_tasks::DuplicateTaskLookup;
use crate::adapter::engine_host::v2_host::test_support::runtime_with_workspace_layout;
use crate::application::task::TaskAddParams;

use super::ci_failure_tasks::{failure, filed_task_ids, snapshot as ci_snapshot};
use super::dependabot_alert_tasks::{
    alert, code_alert, expanded_snapshot, file as file_security, snapshot as security_snapshot,
};

const CHECKOUT: &str = "3333333333333333333333333333333333333333";
const CI_LOG: &str = "ci\tbuild\t2026-08-30T01:00:00Z error: expected 3 arguments, found 2\n";

fn file_ci(runtime: &OrbitRuntime, evidence: Value) -> Value {
    runtime
        .run_deterministic(
            "file_ci_failure_tasks",
            &json!({}),
            &json!({"ci_evidence": evidence}),
            ToolContext::default(),
        )
        .expect("file CI task")
}

fn ci_evidence() -> Value {
    ci_snapshot(vec![failure(
        10,
        "ci",
        "build",
        "cargo build",
        CI_LOG,
        CHECKOUT,
    )])
}

fn seed_manual_task(
    runtime: &OrbitRuntime,
    title: &str,
    description: &str,
    status: TaskStatus,
) -> String {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: description.to_string(),
            acceptance_criteria: vec!["The identified finding is remediated.".to_string()],
            priority: TaskPriority::High,
            task_type: Some(TaskType::Bug),
            status: Some(status),
            ..TaskAddParams::default()
        })
        .expect("seed manual task")
        .id
}

fn seed_manual_ci_task(runtime: &OrbitRuntime, signature: &str) -> String {
    seed_manual_task(
        runtime,
        "Fix red CI: ci / build / cargo build",
        &format!(
            "Workflow: ci\nFailing job: build\nFailing step: cargo build\n\
             Normalized error signature: {signature}"
        ),
        TaskStatus::Backlog,
    )
}

struct FailingBroadLookup<'a> {
    runtime: &'a OrbitRuntime,
}

impl DuplicateTaskLookup for FailingBroadLookup<'_> {
    fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        self.runtime.list_tasks_by_tags(tags)
    }

    fn list_tasks(&self) -> Result<Vec<Task>, OrbitError> {
        Err(injected_lookup_error())
    }
}

struct FailingSecondBroadLookup<'a> {
    runtime: &'a OrbitRuntime,
    broad_calls: Cell<usize>,
}

impl DuplicateTaskLookup for FailingSecondBroadLookup<'_> {
    fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        self.runtime.list_tasks_by_tags(tags)
    }

    fn list_tasks(&self) -> Result<Vec<Task>, OrbitError> {
        let call = self.broad_calls.get();
        self.broad_calls.set(call + 1);
        if call == 0 {
            self.runtime.list_tasks()
        } else {
            Err(injected_lookup_error())
        }
    }
}

fn injected_lookup_error() -> OrbitError {
    OrbitError::Store(format!(
        "injected broad lookup failure ghp_{} {}",
        "E".repeat(36),
        "x".repeat(900)
    ))
}

fn assert_retryable_redacted_lookup_error(error: &str) {
    assert!(error.contains("retryable_error"));
    assert!(error.contains("dedupe_lookup"));
    assert!(error.contains("find_covering_task"));
    assert!(!error.contains(&format!("ghp_{}", "E".repeat(36))));
    assert!(
        error.len() < 1_500,
        "error must stay bounded: {}",
        error.len()
    );
}

#[test]
fn ci_reports_an_untagged_manual_task_with_bounded_match_evidence() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let task_id = seed_manual_ci_task(&runtime, "error: expected <n> arguments, found <n>");

    let output = file_ci(&runtime, ci_evidence());

    assert_eq!(output["filed_count"], json!(0));
    assert_eq!(output["skipped_existing"][0]["task_id"], task_id);
    assert_eq!(
        output["skipped_existing"][0]["match_kind"],
        "material_coverage"
    );
    let matched = output["skipped_existing"][0]["match_evidence"]["matched_fields"]
        .as_array()
        .expect("bounded match evidence");
    assert_eq!(matched.len(), 4);
    assert!(matched.iter().all(|entry| {
        entry["value"]
            .as_str()
            .is_some_and(|value| value.chars().count() <= 160)
    }));
}

#[test]
fn ci_does_not_suppress_a_distinct_error_signature() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    seed_manual_ci_task(&runtime, "error: cannot find type Widget in this scope");

    let output = file_ci(&runtime, ci_evidence());

    assert_eq!(output["filed_count"], json!(1));
    assert_eq!(output["skipped_existing"], json!([]));
}

#[test]
fn completed_ci_repair_is_evidence_not_blanket_immunity_for_a_current_failure() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let completed = seed_manual_task(
        &runtime,
        "Fix red CI: ci / build / cargo build",
        "Workflow: ci\nFailing job: build\nFailing step: cargo build\n\
         Normalized error signature: error: expected <n> arguments, found <n>",
        TaskStatus::Done,
    );

    let output = file_ci(&runtime, ci_evidence());

    assert_eq!(output["filed_count"], json!(1));
    let filed = filed_task_ids(&output);
    assert_ne!(filed[0], completed);
    assert_eq!(
        runtime.get_task(&filed[0]).expect("current task").status,
        TaskStatus::Proposed,
        "the fresh finding must be quarantined for current-relevance pilot assessment"
    );
}

#[test]
fn ci_exact_key_replay_does_not_require_the_broader_lookup() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let evidence = ci_evidence();
    let first = file_ci(&runtime, evidence.clone());
    let task_id = filed_task_ids(&first).remove(0);

    let output = file_ci_failure_tasks_with_lookup(
        &runtime,
        &json!({"ci_evidence": evidence}),
        &FailingBroadLookup { runtime: &runtime },
    )
    .expect("the exact-key fast path must not invoke broad lookup");

    assert_eq!(output["filed_count"], json!(0));
    assert_eq!(output["skipped_existing"][0]["task_id"], task_id);
    assert_eq!(output["skipped_existing"][0]["match_kind"], "exact_key");
}

#[test]
fn ci_duplicate_lookup_failure_is_redacted_retryable_and_writes_nothing() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();

    let error = file_ci_failure_tasks_with_lookup(
        &runtime,
        &json!({"ci_evidence": ci_evidence()}),
        &FailingBroadLookup { runtime: &runtime },
    )
    .expect_err("broad lookup failure must fail closed")
    .to_string();

    assert_retryable_redacted_lookup_error(&error);
    assert!(runtime.list_tasks().expect("list tasks").is_empty());
}

#[test]
fn security_sweep_reports_an_untagged_manual_dependency_owner() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    let task_id = seed_manual_task(
        &runtime,
        "Update time in Cargo.lock",
        "Bump the vulnerable time dependency and regenerate Cargo.lock.",
        TaskStatus::Backlog,
    );

    let output = file_security(
        &runtime,
        security_snapshot(vec![alert(1, "high", "< 1.2.3", "GHSA-manual")], Vec::new()),
        json!({}),
    );

    assert_eq!(output["filed_count"], json!(0));
    assert_eq!(output["skipped_existing"][0]["task_id"], task_id);
    assert_eq!(
        output["skipped_existing"][0]["match_kind"],
        "material_coverage"
    );
    assert_eq!(
        output["skipped_existing"][0]["match_evidence"]["fingerprint"],
        "dependency_title"
    );
}

#[test]
fn security_sweep_does_not_conflate_similar_dependencies_or_manifests() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    seed_manual_task(
        &runtime,
        "Update runtime in Cargo.toml",
        "Package runtime is declared at manifest path Cargo.toml.",
        TaskStatus::Backlog,
    );

    let output = file_security(
        &runtime,
        security_snapshot(
            vec![alert(1, "high", "< 1.2.3", "GHSA-distinct")],
            Vec::new(),
        ),
        json!({}),
    );

    assert_eq!(output["filed_count"], json!(1));
    assert_eq!(output["skipped_existing"], json!([]));
}

#[test]
fn security_sweep_matches_only_the_same_code_alert_identity() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    let task_id = seed_manual_task(
        &runtime,
        "Fix rust/sql-injection in src/db.rs",
        "Code scanning alert #8 reports rule rust/sql-injection at location src/db.rs lines 17-19.",
        TaskStatus::Review,
    );

    let duplicate = file_security(
        &runtime,
        expanded_snapshot(Vec::new(), vec![code_alert(8, "high")], Vec::new()),
        json!({}),
    );
    assert_eq!(duplicate["filed_count"], json!(0));
    assert_eq!(duplicate["skipped_existing"][0]["task_id"], task_id);

    let distinct = file_security(
        &runtime,
        expanded_snapshot(Vec::new(), vec![code_alert(9, "high")], Vec::new()),
        json!({}),
    );
    assert_eq!(distinct["filed_count"], json!(1));
    assert_eq!(distinct["skipped_existing"], json!([]));
}

#[test]
fn done_and_rejected_tasks_do_not_suppress_current_dependency_alerts() {
    for status in [TaskStatus::Done, TaskStatus::Rejected] {
        let (_root, runtime, _repo) = runtime_with_workspace_layout();
        seed_manual_task(
            &runtime,
            "Update time in Cargo.lock",
            "Package time at manifest path Cargo.lock.",
            status,
        );

        let output = file_security(
            &runtime,
            security_snapshot(
                vec![alert(1, "high", "< 1.2.3", "GHSA-recurrence")],
                Vec::new(),
            ),
            json!({}),
        );
        assert_eq!(
            output["filed_count"],
            json!(1),
            "{status} must be closed for dedupe"
        );
    }
}

#[test]
fn every_open_status_suppresses_materially_covered_dependency_work() {
    for status in [
        TaskStatus::Proposed,
        TaskStatus::Backlog,
        TaskStatus::InProgress,
        TaskStatus::Review,
        TaskStatus::Blocked,
        TaskStatus::Someday,
    ] {
        let (_root, runtime, _repo) = runtime_with_workspace_layout();
        seed_manual_task(
            &runtime,
            "Update time in Cargo.lock",
            "Package time at manifest path Cargo.lock.",
            status,
        );

        let output = file_security(
            &runtime,
            security_snapshot(vec![alert(1, "high", "< 1.2.3", "GHSA-open")], Vec::new()),
            json!({}),
        );
        assert_eq!(
            output["filed_count"],
            json!(0),
            "{status} must remain open for dedupe"
        );
    }
}

#[test]
fn later_security_lookup_failure_is_redacted_retryable_and_writes_nothing() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    let snapshot = expanded_snapshot(
        vec![alert(1, "high", "< 1.2.3", "GHSA-pending")],
        vec![code_alert(8, "high")],
        Vec::new(),
    );
    let lookup = FailingSecondBroadLookup {
        runtime: &runtime,
        broad_calls: Cell::new(0),
    };

    let error = file_dependabot_alert_tasks_with_lookup(
        &runtime,
        &json!({"dependabot_snapshot": snapshot}),
        &lookup,
    )
    .expect_err("the second broad lookup must fail the whole action")
    .to_string();

    assert_retryable_redacted_lookup_error(&error);
    assert_eq!(lookup.broad_calls.get(), 2);
    assert!(
        runtime.list_tasks().expect("list tasks").is_empty(),
        "the first candidate must remain pending until all lookups succeed"
    );
}

/// The jrun-20260905-1932 aftermath: manual repairs (ORB-11306 for the macOS
/// regression, ORB-11307 for the Website one) already own both complete
/// findings while older candidates are still starved of investigation budget.
/// Dedupe has to run against the complete findings — not be withheld by the
/// incomplete ones — and the sweep still has to say what it owes.
#[test]
fn manual_repairs_dedupe_complete_findings_while_gaps_stay_visible() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let macos_owner = seed_manual_task(
        &runtime,
        "Fix red CI: Platform / macOS / cargo test",
        "Workflow: Platform\nFailing job: macOS\nFailing step: cargo test\n\
         Normalized error signature: ##[error]linker command failed",
        TaskStatus::Backlog,
    );
    let website_owner = seed_manual_task(
        &runtime,
        "Fix red CI: Website / build / sync website",
        "Workflow: Website\nFailing job: build\nFailing step: sync website\n\
         Normalized error signature: ##[error]sync command not found",
        TaskStatus::Backlog,
    );

    let mut uninvestigated = failure(33_900_000_001, "Platform", "", "", "", "");
    uninvestigated["investigated"] = json!(false);
    uninvestigated["failed_jobs"] = json!([]);
    uninvestigated["log_excerpt"] = json!("");
    let mut evidence = ci_snapshot(vec![
        failure(
            33_986_585_197,
            "Platform",
            "macOS",
            "cargo test",
            "ci\tmacOS\t2026-09-05T19:20:00Z ##[error]linker command failed\n",
            CHECKOUT,
        ),
        failure(
            33_986_582_084,
            "Website",
            "build",
            "sync website",
            "ci\tbuild\t2026-09-05T19:19:00Z ##[error]sync command not found\n",
            CHECKOUT,
        ),
        uninvestigated,
    ]);
    evidence["retryable_errors"] = json!([{
        "stage": "investigation",
        "operation": "investigation_budget",
        "run_id": 33_900_000_001_u64,
        "retryable": true,
        "message": "current failure was not investigated because max_investigated_runs was exhausted",
    }]);

    let output = file_ci(&runtime, evidence);

    assert_eq!(output["filed_count"], json!(0), "{output}");
    let owners = output["skipped_existing"]
        .as_array()
        .expect("skipped existing")
        .iter()
        .map(|entry| entry["task_id"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert_eq!(owners, vec![macos_owner, website_owner]);
    assert_eq!(output["audit"]["existing_task_skips"], json!(2));
    assert_eq!(output["audit"]["deferred_failures"], json!(1));
    assert_eq!(
        output["deferred"][0]["run_id"],
        json!(33_900_000_001_u64),
        "the starved candidate is still owed: {output}"
    );
}
