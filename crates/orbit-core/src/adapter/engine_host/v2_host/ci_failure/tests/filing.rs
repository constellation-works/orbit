//! `file_ci_failure_tasks`: clustering, dedupe, and the endings that must stay
//! distinct.
//!
//! Every test drives the action through `run_deterministic`, which is the only
//! way a job step reaches it.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::{Task, TaskComment, TaskPriority, TaskStatus, TaskType};
use serde_json::{Value, json};
use tempfile::tempdir;

use super::evidence::compiler_findings;
use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::admission::duplicate_tasks::DuplicateTaskLookup;
use crate::adapter::engine_host::v2_host::ci_failure::filing::{
    file_ci_failure_tasks_with_add, file_ci_failure_tasks_with_lookup,
};
use crate::adapter::engine_host::v2_host::test_support::runtime_with_workspace_layout;
use crate::application::task::{TaskAddParams, TaskUpdateParams};

const HEAD: &str = "1111111111111111111111111111111111111111";
pub(super) const CHECKOUT: &str = "3333333333333333333333333333333333333333";
pub(super) const NEXT_HEAD: &str = "4444444444444444444444444444444444444444";

pub(super) fn file(runtime: &OrbitRuntime, input: Value) -> Value {
    runtime
        .run_deterministic(
            "file_ci_failure_tasks",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .expect("file ci failure tasks")
}

pub(super) fn file_error(runtime: &OrbitRuntime, input: Value) -> String {
    runtime
        .run_deterministic(
            "file_ci_failure_tasks",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .expect_err("file ci failure tasks must remain retryable")
        .to_string()
}

/// One current failure, shaped exactly as `collect_ci_evidence` emits it.
pub(in crate::adapter::engine_host::v2_host) fn failure(
    run_id: u64,
    workflow: &str,
    job: &str,
    step: &str,
    log: &str,
    checkout: &str,
) -> Value {
    json!({
        "run_id": run_id,
        "job_id": 900 + run_id,
        "log_job_id": 900 + run_id,
        "checkout_identity": {"state": "observed", "provenance": {"job_id": 900 + run_id, "complete": true}},
        "workflow": workflow,
        "title": format!("{workflow} on {HEAD}"),
        "status": "completed",
        "conclusion": "failure",
        "event": "push",
        "url": format!("https://github.com/acme/orbit/actions/runs/{run_id}"),
        "created_at": "2026-08-30T01:00:00Z",
        "head_branch": "agent-main",
        "ref_kind": "integration",
        "pr_number": Value::Null,
        "pr_url": Value::Null,
        "event_reported_head_sha": HEAD,
        "current_ref_head_sha": HEAD,
        "actual_checkout_shas": [checkout],
        "checkout_evidence": [format!("HEAD is now at {checkout}")],
        "checkout_evidence_scope": "all",
        "investigated": true,
        "log_excerpt": log,
        "log_truncated": false,
        "failed_jobs": [{
            "job_id": 900 + run_id,
            "name": job,
            "conclusion": "failure",
            "url": format!("https://github.com/acme/orbit/actions/runs/{run_id}/job/{}", 900 + run_id),
            "failed_steps": [{"name": step, "conclusion": "failure"}],
        }],
    })
}

pub(in crate::adapter::engine_host::v2_host) fn snapshot(current: Vec<Value>) -> Value {
    let latest = current.clone();
    json!({
        "schema_version": 2,
        "collected": true,
        "outcome_hint": if current.is_empty() { "no_current_failure" } else { "current_failures" },
        "capability": {
            "available": true,
            "authenticated": true,
            "detail": "GitHub CLI is authenticated on this host",
        },
        "repository": {"name": "orbit", "full_name": "acme/orbit", "default_branch": "main"},
        "heads": [{"kind": "integration", "branch": "agent-main", "current_head_sha": HEAD}],
        "latest_runs": latest,
        "current_failures": current,
        "stale_or_superseded": [],
        "in_flight": [],
        "retryable_errors": [],
        "truncation": {"runs_listed": 4, "current_failures_discovered": 1, "notes": []},
        "collected_at": "2026-08-30T02:00:00Z",
    })
}

pub(in crate::adapter::engine_host::v2_host) fn filed_task_ids(output: &Value) -> Vec<String> {
    output["filed"]
        .as_array()
        .expect("filed array")
        .iter()
        .map(|entry| entry["task_id"].as_str().expect("task id").to_string())
        .collect()
}

/// A workspace whose crew roster has no `system` entry: an explicit `[crews]`
/// table naming only `sol`, with `workflow.system_crew` pointed at a name that
/// resolves to nothing so the usual `system`-aliasing fallback does not kick
/// in either.
fn runtime_without_system_crew() -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempdir().expect("create tempdir");
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).expect("global orbit dir");
    std::fs::create_dir_all(&workspace).expect("workspace orbit dir");
    std::fs::write(
        workspace.join("config.toml"),
        r#"[workflow]
default_crew = "sol"
system_crew = "not-a-real-crew"

[crews.sol]
provider = "codex"
model = "gpt-6-sol"
"#,
    )
    .expect("write crew config with no system entry");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");
    (root, runtime)
}

#[test]
fn a_snapshot_that_could_not_look_reports_capability_unavailable_and_files_nothing() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();

    let output = file(
        &runtime,
        json!({"ci_evidence": {
            "schema_version": 2,
            "collected": false,
            "outcome_hint": "capability_unavailable",
            "capability": {
                "available": true,
                "authenticated": false,
                "detail": "GitHub CLI is present but holds no usable credentials on this host",
            },
            "collected_at": "2026-08-30T02:00:00Z",
        }}),
    );

    assert_eq!(output["outcome"], json!("capability_unavailable"));
    assert_eq!(output["filed_count"], json!(0));
    assert_eq!(output["pilot_candidate_count"], json!(0));
    assert_eq!(output["filed"], json!([]));
    assert_eq!(output["clusters"], json!(0));
    // The distinction that matters: this must never read as a clean CI result.
    assert_ne!(output["outcome"], json!("no_current_failure"));
    assert_eq!(output["capability"]["authenticated"], json!(false));
    assert!(
        runtime
            .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
            .expect("list tasks")
            .is_empty()
    );
}

#[test]
fn no_current_failure_is_a_clean_no_op_and_not_a_capability_problem() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();

    let output = file(&runtime, json!({"ci_evidence": snapshot(Vec::new())}));

    assert_eq!(output["outcome"], json!("no_current_failure"));
    assert_eq!(output["filed_count"], json!(0));
    assert_eq!(output["pilot_candidate_count"], json!(0));
    assert_ne!(output["outcome"], json!("capability_unavailable"));
    assert_eq!(output["capability"]["authenticated"], json!(true));
    assert!(
        runtime
            .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
            .expect("list tasks")
            .is_empty()
    );
}

/// The jrun-20260905-2307 regression: `collect_ci_evidence` deliberately
/// filters an in-flight run with an observed failed job but unavailable logs
/// out of `current_failures` (it is retryable, not a repair yet) while still
/// recording a run-scoped error in `retryable_errors`
/// (`incomplete_mixed_state_evidence_stays_retryable_until_logs_are_available`
/// in `orbit-engine`'s collector tests). That error names no row in
/// `current_failures` for the join-by-run-ID `deferred` construction to
/// attach to, so it must not be silently dropped and read as a clean
/// `no_current_failure`.
#[test]
fn incomplete_mixed_state_evidence_stays_retryable_through_filing() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    // Shaped exactly as the collector emits it for this case: the mixed-state
    // run is absent from `current_failures` entirely, and its only trace is
    // the run-scoped retryable error — passed through unmodified, the same
    // way the `file` step of `ci_failure_sweep_pipeline` receives
    // `steps.collect.output.ci_evidence`.
    let mut evidence = snapshot(Vec::new());
    evidence["outcome_hint"] = json!("retryable_error");
    evidence["in_flight"] = json!([{
        "run_id": 40,
        "workflow": "ci",
        "status": "in_progress",
        "head_branch": "agent-main",
    }]);
    evidence["retryable_errors"] = json!([{
        "stage": "investigation",
        "operation": "run_logs",
        "run_id": 40,
        "retryable": true,
        "message": "logs are not available until the job finishes",
    }]);

    let error = file_error(&runtime, json!({"ci_evidence": evidence}));

    assert!(error.contains("retryable_error"));
    assert!(error.contains("run_logs"));
    assert!(error.contains("\"run_id\":40"));
    assert!(
        runtime
            .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
            .expect("list tasks")
            .is_empty(),
        "a mixed-state run with incomplete logs must never be filed as a clean no-op"
    );
}

/// The companion case: a genuinely pending in-flight run that never failed a
/// job carries no retryable error at all (the collector never reads its logs
/// or checkout), so it must remain a clean no-op rather than being swept up
/// by the fix for the mixed-state gap above.
#[test]
fn a_pending_in_flight_run_with_no_failed_jobs_is_still_a_no_op() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut evidence = snapshot(Vec::new());
    evidence["in_flight"] = json!([{
        "run_id": 40,
        "workflow": "ci",
        "status": "in_progress",
        "head_branch": "agent-main",
    }]);

    let output = file(&runtime, json!({"ci_evidence": evidence}));

    assert_eq!(output["outcome"], json!("no_current_failure"));
    assert_eq!(output["filed_count"], json!(0));
    assert_eq!(output["deferred"], json!([]));
    assert!(
        runtime
            .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
            .expect("list tasks")
            .is_empty()
    );
}

#[test]
fn explicit_deferred_evidence_retains_metadata_alongside_filed_tasks() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let log = "ci\ttest\t2026-08-30T01:00:00Z assertion failed: left == right\n";
    let mut evidence = snapshot(vec![failure(
        10,
        "ci",
        "test (ubuntu)",
        "cargo test",
        log,
        CHECKOUT,
    )]);
    evidence["deferred"] = json!([{
        "run_id": 99,
        "url": "https://github.com/acme/orbit/actions/runs/99",
        "workflow": "ci",
        "head_branch": "feature/unverified",
        "ref_kind": "other",
        "investigated": false,
    }]);
    evidence["retryable_errors"] = json!([{
        "stage": "discovery",
        "operation": "remote_branch_head",
        "run_id": 99,
        "retryable": true,
        "message": "candidate branch 'feature/unverified' could not be checked against origin; its failure remains deferred until verified",
    }]);

    let output = file(&runtime, json!({"ci_evidence": evidence}));

    assert_eq!(output["filed_count"], json!(1));
    let deferred = output["deferred"].as_array().expect("deferred entries");
    assert_eq!(deferred.len(), 1);
    assert_eq!(deferred[0]["run_id"], json!(99));
    assert_eq!(
        deferred[0]["url"],
        json!("https://github.com/acme/orbit/actions/runs/99")
    );
    assert_eq!(deferred[0]["workflow"], json!("ci"));
    assert_eq!(deferred[0]["head_branch"], json!("feature/unverified"));
    assert_eq!(deferred[0]["ref_kind"], json!("other"));
    assert_eq!(deferred[0]["investigated"], json!(false));
    assert_eq!(deferred[0]["retryable"], json!(true));
}

#[test]
fn one_regression_across_push_and_pull_request_runs_becomes_one_task() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let log = "ci\tbuild\t2026-08-30T01:00:00Z error: expected 3 arguments, found 2\n";
    // Same workflow, job, step, error, and tested commit — reported once as a
    // push run and once as a pull-request run.
    let mut pull_request = failure(11, "ci", "build", "cargo build", log, CHECKOUT);
    pull_request["event"] = json!("pull_request");
    pull_request["ref_kind"] = json!("pull_request");
    pull_request["pr_number"] = json!(42);

    let output = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![
            failure(10, "ci", "build", "cargo build", log, CHECKOUT),
            pull_request,
            // A genuinely different root cause in the same snapshot.
            failure(12, "lint", "clippy", "cargo clippy", "lint\tclippy\t2026-08-30T01:00:00Z error: unused variable `x`\n", CHECKOUT),
        ])}),
    );

    assert_eq!(output["clusters"], json!(2));
    assert_eq!(output["filed_count"], json!(2));
    let filed = output["filed"].as_array().expect("filed");
    assert_eq!(filed[0]["run_urls"].as_array().expect("urls").len(), 2);
    assert_eq!(filed[1]["run_urls"].as_array().expect("urls").len(), 1);
    assert_ne!(filed[0]["failure_key"], filed[1]["failure_key"]);
}

#[test]
fn a_filed_task_is_a_proposed_bug_carrying_usable_evidence() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let log = "ci\ttest\t2026-08-30T01:00:00Z assertion failed: left == right\n";

    let output = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            10,
            "ci",
            "test (ubuntu)",
            "cargo test",
            log,
            CHECKOUT,
        )])}),
    );

    let task_id = filed_task_ids(&output)
        .first()
        .cloned()
        .expect("one filed task");
    assert_eq!(output["pilot_candidate_count"], json!(1));
    assert_eq!(output["pilot_candidates"][0]["run_ids"], json!([10]));
    assert_eq!(
        output["pilot_candidates"][0]["ref_kinds"],
        json!(["integration"])
    );
    assert_eq!(
        output["pilot_candidates"][0]["head_branches"],
        json!(["agent-main"])
    );
    let task = runtime.get_task(&task_id).expect("read filed task");

    assert_eq!(task.status, TaskStatus::Proposed);
    assert_eq!(task.task_type, orbit_types::task::TaskType::Bug);
    // No `github.*` requirement: the evidence is in the description, so the
    // task ships on the ordinary agent baseline.
    assert!(task.required_tools.is_empty());
    assert!(task.tags.contains(&"ci-failure-sweep".to_string()));
    assert!(
        task.tags
            .iter()
            .any(|tag| tag.starts_with("ci-failure:") && tag.len() > "ci-failure:".len())
    );
    assert!(!task.acceptance_criteria.is_empty());

    let description = &task.description;
    for expected in [
        "ci",
        "test (ubuntu)",
        "cargo test",
        "assertion failed: left == right",
        "https://github.com/acme/orbit/actions/runs/10",
        CHECKOUT,
        HEAD,
    ] {
        assert!(
            description.contains(expected),
            "filed description must carry `{expected}`; got:\n{description}"
        );
    }
    // The three commits stay separately labelled rather than collapsing.
    assert!(description.contains("event-reported head SHA"));
    assert!(description.contains("current head of that ref"));
    assert!(description.contains("commit actually checked out"));
    // Bounds are reported so "no more failures" is never read as "we stopped
    // looking".
    assert!(description.contains("Collection bounds"));
}

#[test]
fn an_excerpt_recovered_from_a_job_log_names_the_supplying_job() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut recovered = failure(
        10,
        "ci",
        "docs",
        "cargo doc",
        "2026-09-06T21:28:07.0459354Z error: public documentation for `connect` links to \
         private item `reject_root_override`\n",
        CHECKOUT,
    );
    // Collection could not read the run-scoped failed-step log and recovered
    // the excerpt from the failed job's own log instead.
    recovered["job_id"] = json!(101_560_010_340_u64);
    recovered["log_job_id"] = recovered["job_id"].clone();
    recovered["failed_jobs"][0]["job_id"] = recovered["job_id"].clone();
    recovered["checkout_identity"]["provenance"]["job_id"] = recovered["job_id"].clone();
    recovered["log_source"] = json!("job_api_log");
    recovered["log_source_jobs"] = json!([{
        "job_id": 101_560_010_340_u64,
        "name": "docs",
        "conclusion": "failure",
        "url": "https://github.com/acme/orbit/actions/runs/10/job/101560010340",
    }]);

    let output = file(&runtime, json!({"ci_evidence": snapshot(vec![recovered])}));

    assert_eq!(output["filed_count"], json!(1));
    let task_id = filed_task_ids(&output).remove(0);
    let description = runtime
        .get_task(&task_id)
        .expect("read filed task")
        .description;
    assert!(
        description.contains("reject_root_override"),
        "the recovered diagnostic must reach the filed task:\n{description}"
    );
    assert!(
        description.contains("evidence from job `docs` (id `101560010340`)")
            && description.contains("job log API"),
        "a whole-job log must not be presented as a failed-step excerpt:\n{description}"
    );
}

#[test]
fn live_run_fixture_files_once_with_complete_actionable_evidence() {
    const RUN_ID: u64 = 33_358_160_088;
    const JOB_ID: u64 = 99_384_177_985;
    const SHA: &str = "2a4cb4e4631a856552d901b6b062fa6596475cc0";
    const RUN_URL: &str = "https://github.com/danieljhkim/orbit/actions/runs/33358160088";
    const JOB_URL: &str =
        "https://github.com/danieljhkim/orbit/actions/runs/33358160088/job/99384177985";
    const TEST: &str = "orbit-cli::routine_root::routine_commands_honor_orbit_root_and_mutate_only_the_selected_root";
    const EXCERPT: &str = "routine command touched isolated HOME at /tmp/.tmpgNchET/empty-home";

    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut live = failure(
        RUN_ID,
        "CI",
        "Rust tests",
        "Run Rust tests",
        &format!(
            "CI\tRust tests\tassertion failed: {TEST} at crates/orbit-cli/tests/routine_root.rs:218: {EXCERPT}\n"
        ),
        SHA,
    );
    live["job_id"] = json!(JOB_ID);
    live["log_job_id"] = json!(JOB_ID);
    live["checkout_identity"]["provenance"]["job_id"] = json!(JOB_ID);
    live["url"] = json!(RUN_URL);
    live["event_reported_head_sha"] = json!(SHA);
    live["current_ref_head_sha"] = json!(SHA);
    live["actual_checkout_shas"] = json!([SHA]);
    live["checkout_evidence"] = json!([format!("HEAD is now at {SHA}")]);
    live["failed_jobs"] = json!([{
        "job_id": JOB_ID,
        "name": "Rust tests",
        "conclusion": "failure",
        "url": JOB_URL,
        "failed_steps": [{"name": "Run Rust tests", "conclusion": "failure"}],
    }]);
    let evidence = snapshot(vec![live]);

    let first = file(&runtime, json!({"ci_evidence": evidence.clone()}));
    assert_eq!(first["outcome"], json!("current_failures"));
    assert_eq!(first["filed_count"], json!(1));
    assert_eq!(first["audit"]["latest_run_ids"], json!([RUN_ID]));
    assert_eq!(first["audit"]["current_failure_run_ids"], json!([RUN_ID]));
    assert_eq!(
        first["audit"]["investigated_failure_run_ids"],
        json!([RUN_ID])
    );
    assert_eq!(first["audit"]["tasks_created"], json!(1));
    let task_id = filed_task_ids(&first).remove(0);
    let task = runtime.get_task(&task_id).expect("read filed task");
    for expected in [
        RUN_URL,
        JOB_URL,
        "33358160088",
        "99384177985",
        "CI",
        "conclusion `failure`",
        "Run Rust tests",
        TEST,
        "crates/orbit-cli/tests/routine_root.rs:218",
        EXCERPT,
        SHA,
        "event-reported head SHA",
        "current head of that ref",
        "commit actually checked out",
    ] {
        assert!(
            task.description.contains(expected),
            "filed task must contain {expected:?}:\n{}",
            task.description
        );
    }

    let second = file(&runtime, json!({"ci_evidence": evidence}));
    assert_eq!(second["outcome"], json!("current_failures"));
    assert_eq!(second["filed_count"], json!(0));
    assert_eq!(second["skipped_existing"][0]["task_id"], json!(task_id));
    assert_eq!(second["audit"]["existing_task_skips"], json!(1));
    assert_eq!(second["audit"]["existing_task_owners"], json!([task_id]));
    assert_eq!(second["audit"]["current_failure_run_ids"], json!([RUN_ID]));
}

#[test]
fn mixed_state_in_progress_failure_files_once_and_repeat_names_the_owner() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut mixed = failure(
        33_979_680_684,
        "CI",
        "Linux tests",
        "Run tests",
        "CI\tLinux tests\tassertion failed in collect.rs\n",
        "8b0a760bae17b6f0aeee9eab47840684697fa812",
    );
    mixed["status"] = json!("in_progress");
    mixed["conclusion"] = Value::Null;
    mixed["url"] = json!("https://github.com/danieljhkim/orbit/actions/runs/33979680684");
    let evidence = snapshot(vec![mixed]);

    let first = file(&runtime, json!({"ci_evidence": evidence.clone()}));
    assert_eq!(first["outcome"], json!("current_failures"));
    assert_eq!(first["filed_count"], json!(1));
    let task_id = filed_task_ids(&first).remove(0);
    let task = runtime.get_task(&task_id).expect("read filed task");
    assert!(task.description.contains("Linux tests"));
    assert!(task.description.contains("33979680684"));

    let second = file(&runtime, json!({"ci_evidence": evidence}));
    assert_eq!(second["outcome"], json!("current_failures"));
    assert_eq!(second["filed_count"], json!(0));
    assert_eq!(second["skipped_existing"][0]["task_id"], json!(task_id));
    assert_eq!(second["audit"]["existing_task_owners"], json!([task_id]));
}

#[test]
fn task_add_failure_is_retryable_and_cannot_persist_a_handled_state() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let evidence = snapshot(vec![failure(
        10,
        "CI",
        "Rust tests",
        "Run Rust tests",
        "CI\tRust tests\tassertion failed: injected filing fault\n",
        CHECKOUT,
    )]);

    let error =
        file_ci_failure_tasks_with_add(&runtime, &json!({"ci_evidence": evidence}), |_params| {
            Err(OrbitError::Execution(
                "injected orbit.task.add failure".to_string(),
            ))
        })
        .expect_err("task creation failure must fail the pipeline")
        .to_string();

    assert!(error.contains("retryable_error"));
    assert!(error.contains("task_creation"));
    assert!(error.contains("orbit.task.add"));
    assert!(error.contains("\"current_failure_run_ids\":[10]"));
    assert!(
        runtime
            .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
            .expect("list tasks")
            .is_empty(),
        "failed task creation must not persist a dedupe owner or handled marker"
    );
}

#[test]
fn a_second_sweep_over_a_still_red_run_does_not_file_a_second_task() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let log = "ci\tbuild\t2026-08-30T01:00:00Z error: expected 3 arguments, found 2\n";
    let evidence = snapshot(vec![failure(
        10,
        "ci",
        "build",
        "cargo build",
        log,
        CHECKOUT,
    )]);

    let first = file(&runtime, json!({"ci_evidence": evidence}));
    let task_id = filed_task_ids(&first)
        .first()
        .cloned()
        .expect("first sweep files one task");

    // An hour later the same run is still red, and the branch has also moved
    // on with the failure unfixed — so the newer run reports a different tested
    // commit. Dedupe is keyed on the root cause, not the commit, precisely so
    // this does not file again.
    let later = snapshot(vec![
        failure(10, "ci", "build", "cargo build", log, CHECKOUT),
        failure(13, "ci", "build", "cargo build", log, NEXT_HEAD),
    ]);
    let second = file(&runtime, json!({"ci_evidence": later}));

    assert_eq!(second["outcome"], json!("current_failures"));
    assert_eq!(second["filed_count"], json!(0));
    assert_eq!(
        second["pilot_candidates"][0]["task_id"],
        json!(task_id.clone()),
        "a proposed task whose prior pilot did not admit it must remain retryable"
    );
    let skipped = second["skipped_existing"].as_array().expect("skipped");
    assert!(!skipped.is_empty());
    assert!(
        skipped
            .iter()
            .all(|entry| entry["task_id"] == json!(task_id.clone())),
        "dedupe must name the open task that already covers the cause: {skipped:?}"
    );
    assert_eq!(
        runtime
            .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
            .expect("list tasks")
            .len(),
        1
    );
}

#[test]
fn a_closed_task_does_not_suppress_a_recurrence() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let log = "ci\tbuild\t2026-08-30T01:00:00Z test suite::probe_case ... FAILED\nci\tbuild\t2026-08-30T01:00:01Z error: assertion failed: left == right\n";
    let evidence = snapshot(vec![failure(
        10,
        "ci",
        "build",
        "cargo build",
        log,
        CHECKOUT,
    )]);

    let first = file(&runtime, json!({"ci_evidence": evidence.clone()}));
    let task_id = filed_task_ids(&first)
        .first()
        .cloned()
        .expect("first sweep files one task");
    runtime
        .update_task(
            &task_id,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                ..TaskUpdateParams::default()
            },
        )
        .expect("admit the first task before completing it");
    runtime
        .update_task(
            &task_id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..TaskUpdateParams::default()
            },
        )
        .expect("close the first task");

    let same_run = file(&runtime, json!({"ci_evidence": evidence.clone()}));
    assert_eq!(same_run["filed_count"], json!(0));
    assert_eq!(
        same_run["skipped_existing"][0]["match_evidence"]["fingerprint"],
        json!("ci_failure_run_id")
    );

    let mut same_commit = failure(12, "ci", "build", "cargo build", log, CHECKOUT);
    same_commit["event_reported_head_sha"] = json!(NEXT_HEAD);
    same_commit["current_ref_head_sha"] = json!(NEXT_HEAD);
    let same_commit = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![same_commit])}),
    );
    assert_eq!(same_commit["filed_count"], json!(0));
    assert_eq!(
        same_commit["skipped_existing"][0]["match_evidence"]["fingerprint"],
        json!("ci_failure_head_sha")
    );

    let mut second_failure = failure(11, "ci", "build", "cargo build", log, NEXT_HEAD);
    second_failure["event_reported_head_sha"] = json!(NEXT_HEAD);
    second_failure["current_ref_head_sha"] = json!(NEXT_HEAD);
    let second = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![second_failure])}),
    );

    assert_eq!(second["filed_count"], json!(1));
    assert_ne!(filed_task_ids(&second).first(), Some(&task_id));
}

#[test]
fn an_open_task_merely_citing_the_head_commit_does_not_suppress_a_ci_failure() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    // A code-scanning sweep runs minutes before the CI sweep on the same head,
    // so its per-alert ledger routinely quotes the current branch head. That is
    // a commit reference, not a description of this failure.
    runtime
        .add_task(TaskAddParams {
            title: "[code-scanning-sweep] rust/path-injection in fs/generation.rs".to_string(),
            description: format!(
                "Alert 41 — `rust/path-injection`, analyzed commit {HEAD} on `agent-main`."
            ),
            acceptance_criteria: vec![
                "The alert is resolved or dismissed with a reason.".to_string(),
            ],
            priority: TaskPriority::Medium,
            task_type: Some(TaskType::Bug),
            status: Some(TaskStatus::Backlog),
            ..TaskAddParams::default()
        })
        .expect("seed the open code-scanning task");

    let log = "ci	Coverage (informational)	2026-08-30T01:00:00Z test update::tests::channel::system_inventory ... FAILED
ci	Coverage (informational)	2026-08-30T01:00:01Z error: Text file busy (os error 26)
";
    let filed = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            10,
            "ci",
            "Coverage (informational)",
            "cargo llvm-cov",
            log,
            CHECKOUT,
        )])}),
    );

    assert_eq!(
        filed["skipped_existing"].as_array().map(Vec::len),
        Some(0),
        "a bare commit reference is not coverage: {:?}",
        filed["skipped_existing"]
    );
    assert_eq!(filed["filed_count"], json!(1));
}

#[test]
fn a_listed_but_uninvestigated_failure_is_not_filed_as_an_evidence_free_task() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut uninvestigated = failure(10, "ci", "build", "cargo build", "", CHECKOUT);
    uninvestigated["investigated"] = json!(false);
    uninvestigated["failed_jobs"] = json!([]);
    uninvestigated["log_excerpt"] = json!("");

    let error = file_error(
        &runtime,
        json!({"ci_evidence": snapshot(vec![uninvestigated])}),
    );

    assert!(error.contains("retryable_error"));
    assert!(error.contains("current_failure_not_investigated"));
    assert!(error.contains("\"current_failure_run_ids\":[10]"));
    assert!(
        runtime
            .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
            .expect("list tasks")
            .is_empty()
    );
}

#[test]
fn a_filed_task_title_carries_the_sweep_prefix_and_the_system_crew() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let log = "ci\ttest\t2026-08-30T01:00:00Z assertion failed: left == right\n";

    let output = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            10,
            "ci",
            "test (ubuntu)",
            "cargo test",
            log,
            CHECKOUT,
        )])}),
    );

    let task_id = filed_task_ids(&output)
        .first()
        .cloned()
        .expect("one filed task");
    let task = runtime.get_task(&task_id).expect("read filed task");

    assert!(
        task.title
            .starts_with("[ci-failure-sweep] Fix red CI: ci / test (ubuntu) / cargo test"),
        "title must carry the sweep prefix followed by the existing rendering: {}",
        task.title
    );
    assert_eq!(task.crew.as_deref(), Some("system"));
}

#[test]
fn an_over_long_cluster_yields_an_intact_prefix_within_the_existing_bound() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let long_workflow = "w".repeat(300);
    let log = "ci\tbuild\t2026-08-30T01:00:00Z error: boom\n";

    let output = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            10,
            &long_workflow,
            "build",
            "cargo build",
            log,
            CHECKOUT,
        )])}),
    );

    let task_id = filed_task_ids(&output)
        .first()
        .cloned()
        .expect("one filed task");
    let task = runtime.get_task(&task_id).expect("read filed task");

    assert!(
        task.title.starts_with("[ci-failure-sweep] Fix red CI: "),
        "prefix must survive truncation intact: {}",
        task.title
    );
    assert!(
        task.title.chars().count() <= 121,
        "title must still respect the existing length bound: {} chars",
        task.title.chars().count()
    );
}

#[test]
fn filing_still_succeeds_in_a_workspace_with_no_system_crew_entry() {
    let (_root, runtime) = runtime_without_system_crew();
    assert!(
        runtime.validate_crew_name(Some("system")).is_err(),
        "fixture must genuinely lack a resolvable system crew"
    );
    let log = "ci\tbuild\t2026-08-30T01:00:00Z error: expected 3 arguments, found 2\n";

    let output = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            10,
            "ci",
            "build",
            "cargo build",
            log,
            CHECKOUT,
        )])}),
    );

    assert_eq!(output["outcome"], json!("current_failures"));
    let task_id = filed_task_ids(&output)
        .first()
        .cloned()
        .expect("filing still succeeds without a system crew");
    let task = runtime.get_task(&task_id).expect("read filed task");
    // [ORB-12717] Creation assigns the workspace's own crew; the unresolvable
    // `system_crew` entry is never written onto a filed task.
    assert_eq!(task.crew.as_deref(), Some("sol"));
}

#[test]
fn the_filing_cap_reports_what_it_left_unfiled() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let failures: Vec<Value> = (0..3)
        .map(|index| {
            failure(
                10 + index,
                &format!("workflow-{index}"),
                "build",
                "cargo build",
                &format!("w\tbuild\t2026-08-30T01:00:00Z error: cause number {index} here\n"),
                CHECKOUT,
            )
        })
        .collect();

    let output = file(
        &runtime,
        json!({"ci_evidence": snapshot(failures), "max_tasks": 1}),
    );

    assert_eq!(output["clusters"], json!(3));
    assert_eq!(output["filed_count"], json!(1));
    assert_eq!(
        output["skipped_over_cap"]
            .as_array()
            .expect("over-cap array")
            .len(),
        2,
        "a cap must be reported, never a silent truncation"
    );
}

/// The store as the filer sees it: every read it makes is tallied.
struct CountingLookup<'a> {
    runtime: &'a OrbitRuntime,
    list_calls: Cell<usize>,
    tag_calls: Cell<usize>,
    comment_reads: RefCell<BTreeMap<String, usize>>,
}

impl DuplicateTaskLookup for CountingLookup<'_> {
    fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        self.tag_calls.set(self.tag_calls.get() + 1);
        self.runtime.list_tasks_by_tags(tags)
    }

    fn list_tasks(&self) -> Result<Vec<Task>, OrbitError> {
        self.list_calls.set(self.list_calls.get() + 1);
        self.runtime.list_tasks()
    }

    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        self.runtime.get_task(task_id)
    }

    fn get_task_comments(&self, task_id: &str) -> Result<Vec<TaskComment>, OrbitError> {
        *self
            .comment_reads
            .borrow_mut()
            .entry(task_id.to_string())
            .or_default() += 1;
        self.runtime.get_task_comments(task_id)
    }
}

/// A red run with several clusters used to re-hydrate the whole task list and
/// re-read every open task's comments once per cluster — and again once per
/// legacy key on a compiler-cause cluster. One filing is one snapshot.
#[test]
fn a_multi_cluster_sweep_hydrates_tasks_once_and_reads_each_open_tasks_comments_once() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut open_ids: Vec<String> = (0..3)
        .map(|index| {
            let task = runtime
                .add_task(TaskAddParams {
                    title: format!("Unrelated backlog item {index}"),
                    description: "Nothing here resembles a CI failure.".to_string(),
                    acceptance_criteria: vec!["Done.".to_string()],
                    priority: TaskPriority::Low,
                    task_type: Some(TaskType::Chore),
                    status: Some(TaskStatus::Backlog),
                    ..TaskAddParams::default()
                })
                .expect("seed open task");
            runtime
                .update_task(
                    &task.id,
                    TaskUpdateParams {
                        comment: Some(format!("Discussion on item {index}.")),
                        ..TaskUpdateParams::default()
                    },
                )
                .expect("comment on open task");
            task.id
        })
        .collect();
    open_ids.sort();
    // Two plain clusters plus one compiler-cause cluster consolidated from
    // three jobs, which also carries three legacy keys to assess.
    let mut failures = vec![
        failure(
            10,
            "ci",
            "build",
            "cargo build",
            "ci\tbuild\t2026-08-30T01:00:00Z error: expected 3 arguments, found 2\n",
            CHECKOUT,
        ),
        failure(
            11,
            "ci",
            "test",
            "cargo test",
            "ci\ttest\t2026-08-30T01:00:00Z thread 'main' panicked at 'boom'\n",
            CHECKOUT,
        ),
    ];
    failures.extend(compiler_findings());
    let lookup = CountingLookup {
        runtime: &runtime,
        list_calls: Cell::new(0),
        tag_calls: Cell::new(0),
        comment_reads: RefCell::new(BTreeMap::new()),
    };

    let output = file_ci_failure_tasks_with_lookup(
        &runtime,
        &json!({"ci_evidence": snapshot(failures)}),
        &lookup,
    )
    .expect("file ci failure tasks");

    assert_eq!(output["clusters"], json!(3), "{output}");
    assert_eq!(output["filed_count"], json!(3), "{output}");
    assert_eq!(
        lookup.list_calls.get(),
        1,
        "the task list must be hydrated once per filing, not once per cluster"
    );
    assert!(
        lookup.tag_calls.get() >= 3,
        "each cluster still runs its own exact-key query: {}",
        lookup.tag_calls.get()
    );
    let comment_reads = lookup.comment_reads.borrow();
    assert_eq!(
        comment_reads.keys().cloned().collect::<Vec<_>>(),
        open_ids,
        "only the open set has its comments read"
    );
    assert!(
        comment_reads.values().all(|reads| *reads == 1),
        "each open task's comments are read once per filing: {comment_reads:?}"
    );
}
