//! `file_ci_failure_tasks`: clustering, dedupe, and the endings that must stay
//! distinct.
//!
//! Every test drives the action through `run_deterministic`, which is the only
//! way a job step reaches it.

use orbit_common::OrbitError;
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};
use tempfile::tempdir;

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::ci_failure_tasks::file_ci_failure_tasks_with_add;
use crate::adapter::engine_host::v2_host::test_support::runtime_with_workspace_layout;
use crate::application::task::TaskUpdateParams;

const HEAD: &str = "1111111111111111111111111111111111111111";
pub(super) const CHECKOUT: &str = "3333333333333333333333333333333333333333";
const NEXT_HEAD: &str = "4444444444444444444444444444444444444444";

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
pub(super) fn failure(
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

pub(super) fn snapshot(current: Vec<Value>) -> Value {
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

pub(super) fn filed_task_ids(output: &Value) -> Vec<String> {
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
model = "gpt-5.6-sol"
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
    assert_eq!(task.crew, None);
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

fn signature_line(description: &str) -> String {
    description
        .lines()
        .find(|line| line.contains("Normalized error signature"))
        .expect("signature line")
        .to_string()
}

fn excerpt_block(description: &str) -> String {
    let start = description
        .find("## Failed-step log excerpt")
        .expect("excerpt heading");
    let rest = &description[start..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    rest[..end].to_string()
}

/// Realistic GitHub failed-step log: `##[group]Run …`, a large `env:` dump,
/// then the trailing compiler diagnostic. Each env line is ~90 bytes so 50
/// lines already sit past the 4,000-byte description budget.
fn realistic_github_step_log(command: &str, env_lines: usize, trailing: &str) -> String {
    let prefix = |msg: &str| format!("build\tRun go build\t2026-08-30T01:00:00Z {msg}");
    let mut out = String::new();
    out.push_str(&prefix(&format!("##[group]Run {command}")));
    out.push('\n');
    out.push_str(&prefix("env:"));
    out.push('\n');
    for index in 0..env_lines {
        out.push_str(&prefix(&format!("  VAR_{index}: {}", "x".repeat(60))));
        out.push('\n');
    }
    out.push_str(&prefix("##[endgroup]"));
    out.push('\n');
    out.push_str(trailing);
    if !trailing.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn dani_10111_style_log(commit_subject: &str, error: Option<&str>) -> String {
    let prefix = |msg: &str| format!("Vulnerability scan\tCheckout\t2026-08-30T01:00:00Z {msg}\n");
    let mut out = String::new();
    out.push_str(&prefix("##[group]Run actions/checkout@v4"));
    out.push_str(&prefix("with:"));
    out.push_str(&prefix("  repository: acme/monodev"));
    out.push_str(&prefix("  token: ***"));
    out.push_str(&prefix("env:"));
    out.push_str(&prefix("  GITHUB_TOKEN: ***"));
    out.push_str(&prefix("##[endgroup]"));
    out.push_str(&prefix("Syncing repository: acme/monodev"));
    out.push_str(&prefix(&format!("HEAD is now at e5c1dc9 {commit_subject}")));
    if let Some(error) = error {
        out.push_str(&prefix(error));
    }
    out
}

fn filed_description(runtime: &OrbitRuntime, log: &str) -> (Value, String) {
    let output = file(
        runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            10,
            "ci",
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
    (output, task.description)
}

#[test]
fn excerpt_keeps_the_run_command_and_trailing_error_not_the_env_dump() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let trailing = concat!(
        "build\tRun go build\t2026-08-30T01:00:00Z ##[command]go build ./...\n",
        "build\tRun go build\t2026-08-30T01:00:00Z ./main.go:10:2: undefined: Foo\n",
        "build\tRun go build\t2026-08-30T01:00:00Z ##[error]Process completed with exit code 1.\n",
    );
    let log = realistic_github_step_log("go build ./...", 80, trailing);
    let error_at = log
        .find("undefined: Foo")
        .expect("fixture must contain the trailing error");
    assert!(
        error_at > 4_000,
        "fixture must place the error past the 4,000-byte description budget, at {error_at}"
    );

    let (_output, description) = filed_description(&runtime, &log);
    let excerpt = excerpt_block(&description);
    assert!(
        excerpt.contains("##[group]Run go build ./..."),
        "excerpt must keep the runner command:\n{excerpt}"
    );
    assert!(
        excerpt.contains("undefined: Foo"),
        "excerpt must carry the trailing error, not a head window:\n{excerpt}"
    );
    assert!(
        excerpt.contains("##[error]Process completed with exit code 1."),
        "excerpt must carry the annotated error:\n{excerpt}"
    );
    assert!(
        !excerpt.contains("VAR_0:"),
        "excerpt must drop the env dump:\n{excerpt}"
    );
}

#[test]
fn excerpt_without_an_error_anchor_says_so_and_still_shows_the_command() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let log = realistic_github_step_log("go build ./...", 80, "");

    let (_output, description) = filed_description(&runtime, &log);
    let excerpt = excerpt_block(&description);
    assert!(
        excerpt.contains("##[group]Run go build ./..."),
        "command line must still be shown:\n{excerpt}"
    );
    assert!(
        excerpt.contains("No error anchor was present in the retained excerpt"),
        "missing anchor must be stated, not implied by dumping env:\n{excerpt}"
    );
    assert!(
        !excerpt.contains("VAR_0:"),
        "env dump must not be presented as evidence:\n{excerpt}"
    );
}

#[test]
fn error_signature_prefers_an_annotated_error_over_a_checkout_commit_message() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let log = dani_10111_style_log(
        "chore: add ci failure sweep routine",
        Some("##[error]GO-2024-2611: yaml: vulnerable dependency"),
    );

    let (_output, description) = filed_description(&runtime, &log);
    let signature = signature_line(&description);
    assert!(
        !signature.to_ascii_lowercase().contains("head is now at"),
        "checkout bookkeeping must not become the signature: {signature}"
    );
    assert!(
        signature.contains("yaml") && signature.contains("vulnerable"),
        "signature must come from the ##[error] line: {signature}"
    );
}

#[test]
fn generic_runner_trailer_does_not_collapse_distinct_unannotated_diagnostics() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let trailer =
        "build\tRun go build\t2026-08-30T01:00:00Z ##[error]Process completed with exit code 1.\n";
    let foo_log = realistic_github_step_log(
        "go build ./...",
        2,
        &format!(
            "build\tRun go build\t2026-08-30T01:00:00Z ./main.go:10:2: undefined: Foo\n{trailer}"
        ),
    );
    let bar_log = realistic_github_step_log(
        "go build ./...",
        2,
        &format!(
            "build\tRun go build\t2026-08-30T01:00:00Z ./main.go:14:2: undefined: Bar\n{trailer}"
        ),
    );

    let first = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![
            failure(10, "ci", "build", "go build", &foo_log, CHECKOUT),
            failure(11, "ci", "build", "go build", &bar_log, CHECKOUT),
        ])}),
    );

    assert_eq!(first["filed_count"], json!(2));
    let filed = first["filed"].as_array().expect("filed");
    assert_ne!(filed[0]["failure_key"], filed[1]["failure_key"]);
    for (task_id, diagnostic) in filed_task_ids(&first).iter().zip(["foo", "bar"]) {
        let description = runtime
            .get_task(task_id)
            .expect("read filed task")
            .description;
        let signature = signature_line(&description).to_ascii_lowercase();
        assert!(
            signature.contains(diagnostic),
            "specific diagnostic must be the signature: {signature}"
        );
        assert!(
            !signature.contains("process completed"),
            "generic runner trailer must not be the signature: {signature}"
        );
    }

    let repeated = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            12, "ci", "build", "go build", &foo_log, NEXT_HEAD,
        )])}),
    );
    assert_eq!(repeated["filed_count"], json!(0));
    assert_eq!(
        repeated["skipped_existing"][0]["failure_key"], filed[0]["failure_key"],
        "the same diagnostic must retain its failure key across commits"
    );
}

#[test]
fn checkout_commit_message_containing_failure_is_not_the_signature() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let log = dani_10111_style_log("chore: add ci failure sweep routine", None);

    let (_output, description) = filed_description(&runtime, &log);
    let signature = signature_line(&description);
    assert!(
        signature.contains("step-name fallback"),
        "bookkeeping-only excerpt must label the step-name fallback: {signature}"
    );
    assert!(
        !signature.to_ascii_lowercase().contains("head is now at"),
        "HEAD is now at <hex> chore: … failure … must not be chosen: {signature}"
    );
}

#[test]
fn same_failure_under_a_different_commit_message_reuses_the_failure_key() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let first_log = dani_10111_style_log(
        "chore: add ci failure sweep routine",
        Some("##[error]GO-2024-2611: yaml: vulnerable dependency"),
    );
    let second_log = dani_10111_style_log(
        "chore: mention failure in a later commit",
        Some("##[error]GO-2024-2611: yaml: vulnerable dependency"),
    );

    let first = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            10, "ci", "build", "cargo build", &first_log, CHECKOUT,
        )])}),
    );
    let task_id = filed_task_ids(&first)
        .first()
        .cloned()
        .expect("first sweep files one task");
    let first_key = first["filed"][0]["failure_key"].clone();

    let second = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            11, "ci", "build", "cargo build", &second_log, NEXT_HEAD,
        )])}),
    );

    assert_eq!(second["filed_count"], json!(0));
    let skipped = second["skipped_existing"].as_array().expect("skipped");
    assert!(
        skipped.iter().any(|entry| {
            entry["task_id"] == json!(task_id.clone()) && entry["failure_key"] == first_key
        }),
        "skip_if_open must suppress the second filing under a different commit message: {skipped:?}"
    );
}

/// Captured Coverage / Collect workspace coverage shape from ORB-11340:
/// passing libtest names that contain `error`/`failure`, then `failures:`,
/// the panic, cargo wrappers, and GitHub's generic exit trailer. Collection
/// often drops the `test … FAILED` line with the middle of the log.
fn orb_11340_style_rust_test_log(passing: &[&str], failing: &str) -> String {
    let prefix = |msg: &str| {
        format!(
            "Coverage (informational)\tCollect workspace coverage\t2026-09-06T00:12:19.7226670Z {msg}\n"
        )
    };
    let mut out = String::new();
    out.push_str(&prefix(
        "##[group]Run cargo llvm-cov --workspace --locked --no-report",
    ));
    for name in passing {
        out.push_str(&prefix(&format!("test {name} ... ok")));
    }
    out.push_str(&prefix(""));
    out.push_str(&prefix("failures:"));
    out.push_str(&prefix(""));
    out.push_str(&prefix(&format!("---- {failing} stdout ----")));
    out.push_str(&prefix(""));
    out.push_str(&prefix(&format!(
        "thread '{failing}' (10411) panicked at crates/orbit-cli/tests/mcp_roundtrip.rs:1416:33:"
    )));
    out.push_str(&prefix(
        "spawn destination-issued command: Os { code: 26, kind: ExecutableFileBusy, message: \"Text file busy\" }",
    ));
    out.push_str(&prefix(
        "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace",
    ));
    out.push_str(&prefix(""));
    out.push_str(&prefix("failures:"));
    out.push_str(&prefix(&format!("    {failing}")));
    out.push_str(&prefix(""));
    out.push_str(&prefix(
        "test result: FAILED. 46 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 81.42s",
    ));
    out.push_str(&prefix(""));
    out.push_str(&prefix(
        "error: test failed, to rerun pass `-p orbit-cli --test mcp_roundtrip`",
    ));
    out.push_str(&prefix(
        "error: process didn't exit successfully: `/home/runner/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo test --tests --manifest-path /home/runner/work/orbit/orbit/Cargo.toml --target-dir /home/runner/work/orbit/orbit/target/llvm-cov-target --workspace --locked` (exit status: 101)",
    ));
    out.push_str(&prefix("##[error]Process completed with exit code 101."));
    out
}

const ORB_11340_PASSING: &[&str] = &[
    "unmanaged_orbit_workspace_env_does_not_bind_mcp",
    "mcp_serve_error_paths_return_tool_errors_and_keep_serving",
    "task_show_is_global_by_default_across_tool_run_and_mcp",
];

const ORB_11340_FAILING: &str = "a_forced_command_ignores_the_command_the_caller_asked_for";

#[test]
fn passing_test_names_with_error_words_are_not_the_signature() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let log = orb_11340_style_rust_test_log(ORB_11340_PASSING, ORB_11340_FAILING);

    let (_output, description) = filed_description(&runtime, &log);
    let signature = signature_line(&description).to_ascii_lowercase();
    assert!(
        signature.contains(ORB_11340_FAILING) && signature.contains("panicked"),
        "signature must be the panic diagnostic: {signature}"
    );
    assert!(
        !signature.contains("mcp_serve_error_paths")
            && !signature.contains("... ok")
            && !signature.contains("keep_serving"),
        "a passing test whose name contains error must not be the signature: {signature}"
    );
    assert!(
        !signature.contains("process completed"),
        "generic runner trailer must not be the signature: {signature}"
    );
}

#[test]
fn distinct_rust_panics_with_the_same_passing_preamble_keep_distinct_keys() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let foo_log = orb_11340_style_rust_test_log(ORB_11340_PASSING, ORB_11340_FAILING);
    let bar_log = orb_11340_style_rust_test_log(
        ORB_11340_PASSING,
        "another_forced_command_ignores_the_command_the_caller_asked_for",
    );

    let first = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![
            failure(10, "ci", "coverage", "collect coverage", &foo_log, CHECKOUT),
            failure(11, "ci", "coverage", "collect coverage", &bar_log, CHECKOUT),
        ])}),
    );

    assert_eq!(first["filed_count"], json!(2));
    let filed = first["filed"].as_array().expect("filed");
    assert_ne!(filed[0]["failure_key"], filed[1]["failure_key"]);
    for (task_id, needle) in filed_task_ids(&first).iter().zip([
        ORB_11340_FAILING,
        "another_forced_command_ignores_the_command_the_caller_asked_for",
    ]) {
        let description = runtime
            .get_task(task_id)
            .expect("read filed task")
            .description;
        let signature = signature_line(&description).to_ascii_lowercase();
        assert!(
            signature.contains(needle) && signature.contains("panicked"),
            "each panic must be its own signature: {signature}"
        );
        assert!(
            !signature.contains("mcp_serve_error_paths"),
            "shared passing preamble must not become the signature: {signature}"
        );
    }

    let renamed_preamble = orb_11340_style_rust_test_log(
        &[
            "renamed_mcp_serve_error_paths_return_tool_errors_and_keep_serving",
            "workspace_init_mcp_config_reaches_a_governed_tool_over_the_real_transport",
        ],
        ORB_11340_FAILING,
    );
    let repeated = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            12,
            "ci",
            "coverage",
            "collect coverage",
            &renamed_preamble,
            NEXT_HEAD,
        )])}),
    );
    assert_eq!(repeated["filed_count"], json!(0));
    assert_eq!(
        repeated["skipped_existing"][0]["failure_key"], filed[0]["failure_key"],
        "the same panic must retain its failure key across passing-test names, run ids, and commits"
    );
}

fn ansi_bold_red(text: &str) -> String {
    format!("\u{1b}[31;1m{text}\u{1b}[0m")
}

fn github_line(job: &str, step: &str, payload: &str) -> String {
    format!("{job}\t{step}\t2026-09-07T07:24:42.8592482Z {payload}\n")
}

/// ORB-11509: nextest cancellation and summary wrap a FAIL line. ANSI styling
/// must not become the signature, and colored/uncolored logs must match.
///
/// `elapsed` is the per-test duration nextest prints in the FAIL line; it
/// differs on every rerun of the same regression.
fn orb_11509_style_nextest_log(colored: bool, failing: &str, elapsed: &str) -> String {
    let job = "Check / Clippy / Test";
    let step = "Run CI guardrails";
    let paint = |text: &str, color: bool| {
        if color {
            ansi_bold_red(text)
        } else {
            text.to_string()
        }
    };
    let mut out = String::new();
    out.push_str(&github_line(job, step, "##[group]Run cargo nextest run"));
    out.push_str(&github_line(
        job,
        step,
        "test mcp_serve_error_paths_return_tool_errors_and_keep_serving ... ok",
    ));
    out.push_str(&github_line(
        job,
        step,
        &format!(
            "{} due to {}: ",
            paint("  Cancelling", colored),
            paint("test failure", colored)
        ),
    ));
    out.push_str(&github_line(job, step, "────────────"));
    out.push_str(&github_line(
        job,
        step,
        &format!(
            "{} [ 177.529s] 2786/4410 tests run: 2785 passed (2 slow), 1 failed, 10 skipped",
            paint("     Summary", colored)
        ),
    ));
    out.push_str(&github_line(
        job,
        step,
        &format!(
            "{} [   {elapsed}] (2786/4410) {} {}",
            paint("        FAIL", colored),
            paint("orbit-cli::output_goldens", colored),
            paint(failing, colored)
        ),
    ));
    out.push_str(&github_line(
        job,
        step,
        "warning: 1624/4410 tests were not run due to test failure (run with --no-fail-fast to run all tests)",
    ));
    out.push_str(&github_line(
        job,
        step,
        &format!("{}: test run failed", paint("error", colored)),
    ));
    out.push_str(&github_line(
        job,
        step,
        "##[error]Process completed with exit code 100.",
    ));
    out
}

/// ORB-11470 / ORB-11467: cargo's colored `error: test failed, to rerun pass`
/// trailer can appear before the panic when the excerpt is a recovered job log.
fn orb_11470_style_macos_log(failing: &str, cargo_before_panic: bool) -> String {
    let job = "macOS Sandbox";
    let step = "Run orbit-exec sandbox tests (real sandbox-exec)";
    let prefix = |payload: &str| github_line(job, step, payload);
    let cargo = format!(
        "{}: test failed, to rerun pass `-p orbit-exec --lib`",
        ansi_bold_red("error")
    );
    let header = format!("---- {failing} stdout ----");
    let panic = format!(
        "thread '{failing}' (14083) panicked at crates/orbit-exec/src/macos_sandbox/tests/compile.rs:454:5:"
    );
    let mut out = String::new();
    out.push_str(&prefix("##[group]Run cargo test -p orbit-exec --locked"));
    out.push_str(&prefix(
        "test macos_sandbox::tests::spawn::spawn_under_macos_sandbox_runs_program_in_provided_cwd ... ok",
    ));
    out.push_str(&prefix("failures:"));
    out.push_str(&prefix(&header));
    if cargo_before_panic {
        out.push_str(&prefix(&cargo));
        out.push_str(&prefix(&panic));
    } else {
        out.push_str(&prefix(&panic));
        out.push_str(&prefix(&cargo));
    }
    out.push_str(&prefix(
        "an explicit denyRead must still outrank the public CA default",
    ));
    out.push_str(&prefix("failures:"));
    out.push_str(&prefix(&format!("    {failing}")));
    out.push_str(&prefix(
        "test result: FAILED. 67 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.57s",
    ));
    out.push_str(&prefix("##[error]Process completed with exit code 101."));
    out
}

/// ORB-11498 / ORB-11502: golden assertion payload quotes github.run.logs help
/// text containing `failed steps`. The failing test name is in the libtest
/// summary list; the panic line is omitted as in a head/tail truncated excerpt.
fn orb_11498_style_golden_log(failing: &str) -> String {
    let job = "Coverage (informational)";
    let step = "Collect workspace coverage";
    let prefix = |payload: &str| github_line(job, step, payload);
    let mut out = String::new();
    out.push_str(&prefix(
        "##[group]Run cargo llvm-cov --workspace --locked --no-report",
    ));
    out.push_str(&prefix(
        "test no_ansi_escapes_under_any_color_configuration ... ok",
    ));
    out.push_str(&prefix(
        r#"        "description": "Read a bounded excerpt of one GitHub Actions run's logs — failed steps by default, or the full log — plus runner checkout evidence. The source stream is drained incrementally; checkout extraction stops after 8 MiB.""#,
    ));
    out.push_str(&prefix(
        r#"  right: "Read a bounded excerpt of one GitHub Actions run's logs — failed steps by default, or the full log — plus runner checkout evidence.""#,
    ));
    out.push_str(&prefix("failures:"));
    out.push_str(&prefix(&format!("    {failing}")));
    out.push_str(&prefix(
        "test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 9.64s",
    ));
    out.push_str(&prefix(
        "error: test failed, to rerun pass `-p orbit-cli --test output_goldens`",
    ));
    out.push_str(&prefix(
        "error: process didn't exit successfully: `/home/runner/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo test --tests` (exit status: 101)",
    ));
    out.push_str(&prefix("##[error]Process completed with exit code 101."));
    out
}

/// ORB-11513: Wrangler colored `[ERROR]` plus the missing-field diagnostic,
/// then GitHub's generic `The process 'npx' failed with exit code`.
pub(super) fn orb_11513_style_wrangler_log(title: &str, detail: &str) -> String {
    let job = "Publish to Cloudflare Pages";
    let step = "Deploy static site";
    let prefix = |payload: &str| github_line(job, step, payload);
    let wrangler_error = format!(
        "\u{1b}[31m✘ \u{1b}[41;31m[\u{1b}[41;97mERROR\u{1b}[41;31m]\u{1b}[0m \u{1b}[1m{title}:\u{1b}[0m"
    );
    let mut out = String::new();
    out.push_str(&prefix(
        "##[group]Run cloudflare/wrangler-action@ebbaa1584979971c8614a24965b4405ff95890e0",
    ));
    out.push_str(&prefix("[command]/usr/local/bin/npm i wrangler@4.129.0"));
    out.push_str(&prefix(
        "[command]/usr/local/bin/npx --no-install wrangler --version",
    ));
    out.push_str(&prefix(
        "[command]/usr/local/bin/npx wrangler pages deploy dist --project-name=orbit-website --branch=main --commit-hash=a93caa13890764380e184d996fa709b1bcbe278c",
    ));
    out.push_str(&prefix(&wrangler_error));
    out.push_str(&prefix(&format!("    - {detail}")));
    out.push_str(&prefix(
        "##[error]The process '/usr/local/bin/npx' failed with exit code 1",
    ));
    out.push_str(&prefix("##[error]🚨 Action failed"));
    out
}

#[test]
fn colored_and_uncolored_nextest_cancellation_share_the_fail_identity() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    const FAILING: &str = "plain_and_json_forms_match_their_goldens";
    let colored = orb_11509_style_nextest_log(true, FAILING, "1.399s");
    let plain = orb_11509_style_nextest_log(false, FAILING, "1.399s");

    let first = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            10, "CI", "Check / Clippy / Test", "Run CI guardrails", &colored, CHECKOUT,
        )])}),
    );
    assert_eq!(first["filed_count"], json!(1));
    let task_id = filed_task_ids(&first).remove(0);
    let signature = signature_line(
        &runtime
            .get_task(&task_id)
            .expect("read filed task")
            .description,
    )
    .to_ascii_lowercase();
    assert!(
        signature.contains(FAILING) && signature.contains("fail"),
        "nextest FAIL line must be the signature: {signature}"
    );
    assert!(
        !signature.contains("cancelling")
            && !signature.contains("process completed")
            && !signature.contains("test run failed")
            && !signature.contains('\u{1b}'),
        "cancellation, cargo trailer, and ANSI must not be the signature: {signature}"
    );
    assert!(
        runtime
            .get_task(&task_id)
            .expect("read filed task")
            .description
            .contains("Cancelling"),
        "raw colored excerpt must remain in the description"
    );

    let repeated = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            11, "CI", "Check / Clippy / Test", "Run CI guardrails", &plain, NEXT_HEAD,
        )])}),
    );
    assert_eq!(repeated["filed_count"], json!(0));
    assert_eq!(
        repeated["skipped_existing"][0]["failure_key"], first["filed"][0]["failure_key"],
        "colored and uncolored nextest FAIL logs must share a failure key"
    );
}

/// A colored log can reach the sweep with an escape sequence cut short — the
/// stripper must not split the multi-byte character that follows it.
#[test]
fn a_truncated_escape_before_a_multibyte_character_still_files() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let log = format!(
        "{}{}{}",
        github_line("build", "cargo test", "##[group]Run cargo nextest run"),
        github_line("build", "cargo test", "\u{1b}────────────"),
        github_line(
            "build",
            "cargo test",
            "    FAIL [   1.399s] orbit-core truncated_escape_case",
        ),
    );

    let (_output, description) = filed_description(&runtime, &log);
    let signature = signature_line(&description).to_ascii_lowercase();
    assert!(
        signature.contains("truncated_escape_case"),
        "the FAIL identity must survive a truncated escape: {signature}"
    );
}

#[test]
fn nextest_fail_durations_do_not_fragment_one_regression() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    const FAILING: &str = "plain_and_json_forms_match_their_goldens";
    let first_run = orb_11509_style_nextest_log(true, FAILING, "1.399s");
    let rerun = orb_11509_style_nextest_log(true, FAILING, "2.004s");

    let first = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            10, "CI", "Check / Clippy / Test", "Run CI guardrails", &first_run, CHECKOUT,
        )])}),
    );
    assert_eq!(first["filed_count"], json!(1));

    let repeated = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            11, "CI", "Check / Clippy / Test", "Run CI guardrails", &rerun, NEXT_HEAD,
        )])}),
    );
    assert_eq!(
        repeated["filed_count"],
        json!(0),
        "a rerun of the same test must not file a second task"
    );
    assert_eq!(
        repeated["skipped_existing"][0]["failure_key"], first["filed"][0]["failure_key"],
        "the per-test duration must not change the failure key"
    );
}

#[test]
fn distinct_nextest_fail_lines_in_the_same_job_keep_distinct_keys() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let foo =
        orb_11509_style_nextest_log(true, "plain_and_json_forms_match_their_goldens", "1.399s");
    let bar = orb_11509_style_nextest_log(false, "another_golden_does_not_match", "2.004s");

    let output = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![
            failure(10, "CI", "Check / Clippy / Test", "Run CI guardrails", &foo, CHECKOUT),
            failure(11, "CI", "Check / Clippy / Test", "Run CI guardrails", &bar, CHECKOUT),
        ])}),
    );
    assert_eq!(output["filed_count"], json!(2));
    let filed = output["filed"].as_array().expect("filed");
    assert_ne!(filed[0]["failure_key"], filed[1]["failure_key"]);
}

#[test]
fn cargo_test_failed_trailer_does_not_outrank_the_panic() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    const FAILING: &str = "macos_sandbox::tests::compile::compiled_codex_profile_reads_public_ca_material_but_not_private_credentials";
    let trailer_first = orb_11470_style_macos_log(FAILING, true);
    let panic_first = orb_11470_style_macos_log(FAILING, false);

    let first = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            10,
            "macOS Platform",
            "macOS Sandbox",
            "Run orbit-exec sandbox tests (real sandbox-exec)",
            &trailer_first,
            CHECKOUT,
        )])}),
    );
    assert_eq!(first["filed_count"], json!(1));
    let signature = signature_line(
        &runtime
            .get_task(&filed_task_ids(&first)[0])
            .expect("read filed task")
            .description,
    )
    .to_ascii_lowercase();
    assert!(
        signature.contains(FAILING) && signature.contains("panicked"),
        "panic must outrank the cargo trailer: {signature}"
    );
    assert!(
        !signature.contains("to rerun pass") && !signature.contains("process completed"),
        "cargo/github wrappers must not be the signature: {signature}"
    );

    let repeated = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            11,
            "macOS Platform",
            "macOS Sandbox",
            "Run orbit-exec sandbox tests (real sandbox-exec)",
            &panic_first,
            NEXT_HEAD,
        )])}),
    );
    assert_eq!(repeated["filed_count"], json!(0));
    assert_eq!(
        repeated["skipped_existing"][0]["failure_key"],
        first["filed"][0]["failure_key"]
    );
}

#[test]
fn golden_assertion_help_text_does_not_outrank_the_failing_test_name() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    const FAILING: &str = "plain_and_json_forms_match_their_goldens";
    let log = orb_11498_style_golden_log(FAILING);

    let first = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            10,
            "CI",
            "Coverage (informational)",
            "Collect workspace coverage",
            &log,
            CHECKOUT,
        )])}),
    );
    assert_eq!(first["filed_count"], json!(1));
    let task_id = filed_task_ids(&first).remove(0);
    let description = runtime
        .get_task(&task_id)
        .expect("read filed task")
        .description;
    let signature = signature_line(&description).to_ascii_lowercase();
    assert!(
        signature.contains(FAILING),
        "listed golden test name must be the signature: {signature}"
    );
    assert!(
        !signature.contains("failed steps")
            && !signature.contains("bounded excerpt")
            && !signature.contains("to rerun pass"),
        "assertion payload and cargo trailer must not be the signature: {signature}"
    );
    assert!(
        description.contains("failed steps by default"),
        "raw assertion payload must remain in the excerpt"
    );

    let repeated = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            11,
            "CI",
            "Coverage (informational)",
            "Collect workspace coverage",
            &log,
            NEXT_HEAD,
        )])}),
    );
    assert_eq!(repeated["filed_count"], json!(0));
}

#[test]
fn wrangler_error_outranks_generic_npx_process_failed() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let missing_name = orb_11513_style_wrangler_log(
        "Running configuration file validation for Pages",
        "Missing top-level field \"name\" in configuration file.",
    );
    let missing_pages = orb_11513_style_wrangler_log(
        "Failed to publish your Function",
        "Pages build output directory is missing.",
    );

    let first = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![failure(
            10,
            "Website",
            "Publish to Cloudflare Pages",
            "Deploy static site",
            &missing_name,
            CHECKOUT,
        )])}),
    );
    assert_eq!(first["filed_count"], json!(1));
    let signature = signature_line(
        &runtime
            .get_task(&filed_task_ids(&first)[0])
            .expect("read filed task")
            .description,
    )
    .to_ascii_lowercase();
    assert!(
        signature.contains("configuration file validation")
            || signature.contains("missing top-level field"),
        "wrangler diagnostic must outrank npx process-failed: {signature}"
    );
    assert!(
        !signature.contains("usr/local/bin/npx") && !signature.contains("action failed"),
        "generic process/action trailers must not be the signature: {signature}"
    );

    let mut second_failure = failure(
        11,
        "Website",
        "Publish to Cloudflare Pages",
        "Deploy static site",
        &missing_pages,
        NEXT_HEAD,
    );
    second_failure["event_reported_head_sha"] = json!(NEXT_HEAD);
    second_failure["current_ref_head_sha"] = json!(NEXT_HEAD);
    let second = file(
        &runtime,
        json!({"ci_evidence": snapshot(vec![second_failure])}),
    );
    assert_eq!(second["filed_count"], json!(1));
    assert_ne!(
        first["filed"][0]["failure_key"], second["filed"][0]["failure_key"],
        "distinct wrangler diagnostics in the same job must not collapse"
    );
}

#[test]
fn generic_only_truncated_excerpt_labels_step_name_fallback() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let log = format!(
        "{}{}{}",
        github_line("build", "cargo test", "##[group]Run cargo test"),
        github_line(
            "build",
            "cargo test",
            "error: test failed, to rerun pass `-p orbit-exec --lib`",
        ),
        github_line(
            "build",
            "cargo test",
            "##[error]Process completed with exit code 101.",
        ),
    );

    let (_output, description) = filed_description(&runtime, &log);
    let signature = signature_line(&description);
    assert!(
        signature.contains("step-name fallback"),
        "generic-only excerpt must label fallback uncertainty: {signature}"
    );
    assert!(
        description.contains("test failed, to rerun pass")
            && description.contains("Process completed with exit code 101."),
        "raw generic trailers must remain in the excerpt:\n{description}"
    );
}

#[test]
fn query_error_prevents_filing_and_remains_retryable() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut missing_log = failure(10, "ci", "build", "Run CI guardrails", "", CHECKOUT);
    missing_log["log_excerpt"] = json!("");
    let mut evidence = snapshot(vec![missing_log]);
    evidence["query_errors"] = json!([
        {
            "query": "run_logs",
            "run_id": "10",
            "error": "HTTP 404: Not Found — logs for this run are no longer available"
        },
        {
            "query": "run_list",
            "branch": "other",
            "error": "unrelated list failure"
        }
    ]);

    let error = file_error(&runtime, json!({"ci_evidence": evidence}));
    assert!(error.contains("retryable_error"));
    assert!(error.contains("run_logs"));
    assert!(error.contains("logs for this run are no longer available"));
    assert!(error.contains("\"current_failure_run_ids\":[10]"));
    assert!(
        runtime
            .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
            .expect("list tasks")
            .is_empty()
    );
}

/// One uninvestigated run, shaped as collection leaves it when the budget runs
/// out: a URL and a verdict, no job, step, or log.
fn deferred_failure(run_id: u64, workflow: &str, branch: &str) -> Value {
    let mut failure = failure(run_id, workflow, "", "", "", "");
    failure["investigated"] = json!(false);
    failure["failed_jobs"] = json!([]);
    failure["log_excerpt"] = json!("");
    failure["actual_checkout_shas"] = json!([]);
    failure["checkout_evidence"] = json!([]);
    failure["head_branch"] = json!(branch);
    failure["ref_kind"] = json!("pull_request");
    failure
}

fn budget_error(run_id: u64) -> Value {
    json!({
        "stage": "investigation",
        "operation": "investigation_budget",
        "run_id": run_id,
        "retryable": true,
        "message": "current failure was not investigated because max_investigated_runs was exhausted",
    })
}

/// The jrun-20260905-1932 regression: fourteen candidates, three of them fully
/// evidenced, and the other eleven starved of investigation budget. The three
/// complete findings are real, filable defects and must not be withheld
/// because their neighbours are incomplete.
#[test]
fn complete_findings_file_while_incomplete_ones_stay_deferred() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut current = vec![
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
        failure(
            33_986_085_270,
            "Pi",
            "macOS",
            "cargo build",
            "ci\tmacOS\t2026-09-05T19:10:00Z ##[error]could not compile orbit-pi\n",
            CHECKOUT,
        ),
    ];
    let deferred_ids = (0..11_u64)
        .map(|index| 33_900_000_000 + index)
        .collect::<Vec<_>>();
    for run_id in &deferred_ids {
        current.push(deferred_failure(
            *run_id,
            "Platform",
            "orbit/ORB-11200-older",
        ));
    }
    let mut evidence = snapshot(current);
    evidence["retryable_errors"] = json!(
        deferred_ids
            .iter()
            .map(|run_id| budget_error(*run_id))
            .collect::<Vec<_>>()
    );

    let output = file(&runtime, json!({"ci_evidence": evidence}));

    assert_eq!(output["outcome"], json!("current_failures"));
    assert_eq!(output["filed_count"], json!(3), "{output}");
    assert_eq!(filed_task_ids(&output).len(), 3);
    // The durable outcome is the tasks themselves, not the report.
    assert_eq!(
        runtime
            .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
            .expect("list tasks")
            .len(),
        3
    );

    let deferred = output["deferred"].as_array().expect("deferred array");
    assert_eq!(deferred.len(), 11);
    assert_eq!(deferred[0]["run_id"], json!(33_900_000_000_u64));
    assert_eq!(deferred[0]["investigated"], json!(false));
    assert_eq!(deferred[0]["retryable"], json!(true));
    assert_eq!(
        deferred[0]["reasons"][0]["operation"],
        json!("investigation_budget")
    );

    let audit = &output["audit"];
    assert_eq!(audit["current_failures"], json!(14));
    assert_eq!(audit["investigated_failures"], json!(3));
    assert_eq!(audit["tasks_created"], json!(3));
    assert_eq!(audit["deferred_failures"], json!(11));
    assert_eq!(audit["retryable_errors"], json!(11));
    assert_eq!(
        audit["deferred_failure_run_ids"]
            .as_array()
            .expect("deferred ids")
            .len(),
        11
    );
}

/// The boundary the partial path must not cross. A listing that failed may be
/// the one holding the newer run that would have superseded a finding, so a
/// snapshot-wide error still withholds everything — including findings that
/// look complete.
#[test]
fn a_snapshot_wide_discovery_error_still_withholds_a_complete_finding() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut evidence = snapshot(vec![failure(
        10,
        "ci",
        "build",
        "cargo build",
        "ci\tbuild\t2026-08-30T01:00:00Z ##[error]expected 3 arguments\n",
        CHECKOUT,
    )]);
    evidence["retryable_errors"] = json!([
        {
            "stage": "discovery",
            "operation": "run_list",
            "run_id": Value::Null,
            "retryable": true,
            "message": "HTTP 502: Bad Gateway",
        },
        budget_error(11),
    ]);

    let error = file_error(&runtime, json!({"ci_evidence": evidence}));

    assert!(error.contains("retryable_error"));
    assert!(error.contains("run_list"));
    // The run-scoped error travels with it, so one payload explains the sweep.
    assert!(error.contains("investigation_budget"));
    assert!(
        runtime
            .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
            .expect("list tasks")
            .is_empty()
    );
}

/// A finding whose own run carries an error is not filed from partial
/// evidence: per-finding requirements are unchanged, and the gap is stated
/// rather than papered over.
#[test]
fn a_finding_whose_own_run_failed_a_query_is_deferred_not_filed_from_partial_evidence() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut evidence = snapshot(vec![
        failure(
            10,
            "ci",
            "build",
            "cargo build",
            "ci\tbuild\t2026-08-30T01:00:00Z ##[error]expected 3 arguments\n",
            CHECKOUT,
        ),
        failure(
            11,
            "Website",
            "deploy",
            "sync website",
            "ci\tdeploy\t2026-08-30T01:00:00Z ##[error]sync command not found\n",
            CHECKOUT,
        ),
    ]);
    evidence["retryable_errors"] = json!([
        {
            "stage": "registration",
            "operation": "checkout_evidence",
            "run_id": 11,
            "retryable": true,
            "message": "checkout evidence scan reached its hard limit; actual checkout identity is incomplete",
        }
    ]);

    let output = file(&runtime, json!({"ci_evidence": evidence}));

    assert_eq!(output["filed_count"], json!(1));
    assert_eq!(output["filed"][0]["workflow"], json!("ci"));
    let deferred = output["deferred"].as_array().expect("deferred array");
    assert_eq!(deferred.len(), 1);
    assert_eq!(deferred[0]["run_id"], json!(11));
    assert_eq!(
        deferred[0]["investigated"],
        json!(true),
        "the run was investigated; its evidence is what is incomplete"
    );
    assert_eq!(
        deferred[0]["reasons"][0]["operation"],
        json!("checkout_evidence")
    );
}

fn two_job_findings() -> Vec<Value> {
    let first = failure(
        10,
        "CI",
        "Clippy",
        "Run guardrails",
        "error: unused import",
        CHECKOUT,
    );
    let mut second = failure(
        10,
        "CI",
        "Coverage",
        "Collect coverage",
        "test output_goldens FAILED",
        NEXT_HEAD,
    );
    second["job_id"] = json!(920);
    second["log_job_id"] = json!(920);
    second["failed_jobs"][0]["job_id"] = json!(920);
    second["checkout_identity"]["provenance"]["job_id"] = json!(920);
    vec![first, second]
}

#[test]
fn two_failed_jobs_file_distinct_correct_findings_regardless_of_order() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let findings = two_job_findings();
    let first = file(&runtime, json!({"ci_evidence": snapshot(findings.clone())}));
    assert_eq!(first["filed_count"], 2);
    let ids = filed_task_ids(&first);
    let clippy = runtime.get_task(&ids[0]).expect("clippy task");
    let coverage = runtime.get_task(&ids[1]).expect("coverage task");
    assert!(clippy.title.contains("Clippy"));
    assert!(clippy.description.contains("unused import"));
    assert!(!clippy.description.contains("output_goldens"));
    assert!(coverage.title.contains("Coverage"));
    assert!(coverage.description.contains("output_goldens"));
    assert!(!coverage.description.contains("unused import"));
    assert_ne!(
        first["filed"][0]["failure_key"],
        first["filed"][1]["failure_key"]
    );
    assert_eq!(first["filed"][0]["tested_commit"], CHECKOUT);
    assert_eq!(first["filed"][1]["tested_commit"], NEXT_HEAD);
    let mut reversed = findings;
    reversed.reverse();
    let second = file(&runtime, json!({"ci_evidence": snapshot(reversed)}));
    assert_eq!(second["filed_count"], 0);
    assert_eq!(
        second["skipped_existing"].as_array().expect("skips").len(),
        2
    );
}

#[test]
fn one_jobs_retryable_error_defers_only_that_job() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut findings = two_job_findings();
    findings[0]["investigated"] = json!(false);
    let mut evidence = snapshot(findings);
    evidence["retryable_errors"] = json!([{
        "run_id": 10, "job_id": 910, "operation": "run_logs", "message": "job log unavailable",
    }]);
    let output = file(&runtime, json!({"ci_evidence": evidence}));
    assert_eq!(output["filed_count"], 1);
    assert_eq!(output["filed"][0]["job"], "Coverage");
    assert_eq!(output["deferred"][0]["job_id"], 910);
    assert_eq!(output["deferred"][0]["reasons"][0]["job_id"], 910);
    assert_eq!(output["deferred"].as_array().expect("deferred").len(), 1);
}

#[test]
fn unbound_legacy_and_incomplete_job_snapshots_require_recollection() {
    for defect in [
        "legacy",
        "fallback",
        "checkout",
        "truncated",
        "missing",
        "steps",
    ] {
        let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
        let mut findings = two_job_findings();
        findings.truncate(1);
        let mut evidence = snapshot(findings);
        let finding = &mut evidence["current_failures"][0];
        match defect {
            "fallback" => {
                finding["log_source"] = json!("job_api_log");
                finding["log_source_jobs"] = json!([{"job_id": 920}]);
            }
            "checkout" => finding["checkout_identity"]["provenance"]["job_id"] = json!(920),
            "truncated" => finding["log_truncated"] = json!(true),
            "missing" => finding["log_excerpt"] = json!(""),
            "steps" => {
                finding["failed_jobs"][0]["failed_steps"] = json!([{"name": "A"}, {"name": "B"}])
            }
            _ => evidence["schema_version"] = json!(1),
        }
        let error = file_error(&runtime, json!({"ci_evidence": evidence}));
        assert!(error.contains("job_evidence_identity"), "{defect}: {error}");
        assert!(
            runtime
                .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
                .expect("tasks")
                .is_empty()
        );
    }
}

#[test]
fn legacy_multi_job_snapshot_cannot_label_coverage_log_as_clippy() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let findings = two_job_findings();
    let mut combined = findings[0].clone();
    combined["failed_jobs"] =
        json!([findings[0]["failed_jobs"][0], findings[1]["failed_jobs"][0],]);
    combined["log_excerpt"] = json!("Coverage\tCollect coverage\ttest output_goldens FAILED\n");
    let mut evidence = snapshot(vec![combined]);
    evidence["schema_version"] = json!(1);
    let error = file_error(&runtime, json!({"ci_evidence": evidence}));
    assert!(error.contains("legacy run-scoped evidence"), "{error}");
    assert!(
        runtime
            .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
            .expect("tasks")
            .is_empty()
    );
}

#[test]
fn complete_units_from_long_logs_file_and_dedupe_without_using_display_noise() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut findings = two_job_findings();
    for finding in &mut findings {
        let text = finding["log_excerpt"]
            .as_str()
            .expect("diagnostic")
            .to_string();
        finding["diagnostic_unit"] = json!({"kind": "runner_command", "complete": true,
            "job_id": finding["job_id"], "step": finding["failed_jobs"][0]["failed_steps"][0]["name"],
            "text": text});
        finding["log_excerpt"] = json!("setup error: unrelated_setup\n[... omitted ...]\ncleanup");
        finding["log_truncated"] = json!(true);
        finding["log_source_complete"] = json!(true);
    }
    let first = file(&runtime, json!({"ci_evidence": snapshot(findings.clone())}));
    assert_eq!(first["filed_count"], 2);
    let ids = filed_task_ids(&first);
    for (id, expected) in ids.iter().zip(["unused import", "output_goldens"]) {
        let task = runtime.get_task(id).expect("task");
        assert!(task.description.contains(expected));
        assert!(
            task.description
                .contains("collection display was truncated")
        );
        assert!(!task.description.contains("unrelated_setup"));
    }
    findings.reverse();
    let second = file(&runtime, json!({"ci_evidence": snapshot(findings)}));
    assert_eq!(second["filed_count"], 0);
    assert_eq!(
        second["skipped_existing"]
            .as_array()
            .expect("deduped")
            .len(),
        2
    );
}

#[test]
fn incomplete_or_foreign_units_cannot_override_truncated_display() {
    for fault in ["job", "step", "incomplete", "source", "generic", "oversize"] {
        let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
        let mut findings = two_job_findings();
        findings.truncate(1);
        let finding = &mut findings[0];
        finding["log_truncated"] = json!(true);
        finding["diagnostic_unit"] = json!({"kind": "runner_command", "complete": true,
            "job_id": finding["job_id"], "step": finding["failed_jobs"][0]["failed_steps"][0]["name"],
            "text": "error: concrete diagnostic"});
        match fault {
            "job" => finding["diagnostic_unit"]["job_id"] = json!(999),
            "step" => finding["diagnostic_unit"]["step"] = json!("Other step"),
            "incomplete" => finding["diagnostic_unit"]["complete"] = json!(false),
            "source" => finding["log_source_complete"] = json!(false),
            "generic" => {
                finding["diagnostic_unit"]["text"] =
                    json!("##[error]Process completed with exit code 101.")
            }
            _ => finding["diagnostic_unit"]["text"] = json!("x".repeat(262_145)),
        }
        let error = file_error(&runtime, json!({"ci_evidence": snapshot(findings)}));
        assert!(error.contains("job_evidence_identity"), "{fault}: {error}");
    }
}

fn compiler_findings() -> Vec<Value> {
    ["macOS", "Clippy", "Coverage"]
        .into_iter()
        .enumerate()
        .map(|(index, job)| {
            let log = format!(
                concat!(
                    "##[group]Run cargo check\n",
                    "    Compiling thiserror v2.0.17\n",
                    "    Checking error_stack v1.0.0\n",
                    "error: process didn't exit successfully: `rustc {}` (exit status: 1)\n",
                    "\x1b[1;31merror[E0062]\x1b[0m: field `owner_machine_id` specified more than once\n",
                    "  --> crates/orbit-core/src/ci_sweep.rs:275:13\n",
                    "   |\n275 | owner_machine_id: None,\n",
                    "   | ^^^^^^^^^^^^^^^^ used more than once\n",
                    "error: could not compile `orbit-core` due to 1 previous error\n",
                    "##[error]Process completed with exit code 101.\n",
                ),
                "--extern error_helper=/tmp/build/é ".repeat(1500)
            );
            let mut finding = failure(70 + index as u64, "CI", job, job, &log, CHECKOUT);
            finding["diagnostic_unit"] = json!({"kind": "runner_command", "complete": true,
                "job_id": finding["job_id"], "step": job, "text": log});
            finding["log_source_complete"] = json!(true);
            finding["log_truncated"] = json!(true);
            finding["log_excerpt"] = json!("setup error: unrelated display noise");
            finding
        })
        .collect()
}

#[test]
fn compiler_cause_consolidates_jobs_and_keeps_actionable_excerpt() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut findings = compiler_findings();
    let output = file(&runtime, json!({"ci_evidence": snapshot(findings.clone())}));
    assert_eq!(output["filed_count"], 1, "{output}");
    assert_eq!(output["clusters"], 1);
    let id = filed_task_ids(&output).remove(0);
    let task = runtime.get_task(&id).expect("compiler owner");
    let signature = signature_line(&task.description);
    assert!(
        signature.contains("error[e0062]: field `owner_machine_id`"),
        "{signature}"
    );
    assert!(!signature.contains('\x1b'));
    let excerpt = excerpt_block(&task.description);
    assert!(excerpt.contains("error[E0062]"));
    assert!(excerpt.contains("ci_sweep.rs:275:13"));
    assert!(excerpt.len() < 4_300, "{}", excerpt.len());
    for finding in &findings {
        assert!(
            task.description
                .contains(finding["failed_jobs"][0]["name"].as_str().expect("name"))
        );
        assert!(task.description.contains(&finding["job_id"].to_string()));
        assert!(task.description.contains(&finding["run_id"].to_string()));
    }
    assert!(task.description.contains(CHECKOUT));
    findings.reverse();
    let repeated = file(&runtime, json!({"ci_evidence": snapshot(findings)}));
    assert_eq!(repeated["filed_count"], 0);
    assert_eq!(repeated["skipped_existing"][0]["task_id"], id);
}

#[test]
fn compiler_causes_with_shared_command_and_location_remain_separate() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut findings = compiler_findings();
    for (index, finding) in findings.iter_mut().enumerate() {
        let text = finding["diagnostic_unit"]["text"].as_str().expect("log");
        finding["diagnostic_unit"]["text"] =
            json!(text.replace("owner_machine_id", &format!("field_{index}")));
    }
    let first = file(&runtime, json!({"ci_evidence": snapshot(findings.clone())}));
    assert_eq!(first["filed_count"], 3);
    findings.reverse();
    let repeated = file(&runtime, json!({"ci_evidence": snapshot(findings)}));
    assert_eq!(repeated["filed_count"], 0);
    let owners: std::collections::BTreeSet<_> = repeated["skipped_existing"]
        .as_array()
        .expect("skips")
        .iter()
        .map(|entry| entry["task_id"].as_str().expect("id"))
        .collect();
    assert_eq!(owners.len(), 3);
}

#[test]
fn compiler_legacy_keys_follow_rejected_owners_only_for_the_original_source() {
    use crate::application::task::TaskAddParams;

    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let findings = compiler_findings();
    let owner = runtime
        .add_task(TaskAddParams {
            title: "Repair the compiler initializer".to_string(),
            description: "Canonical repair explicitly named by the rejected observations."
                .to_string(),
            ..TaskAddParams::default()
        })
        .expect("owner");
    // Shipped workflow/job/step/first-marker digests for these exact logs.
    let keys = ["2c5df683bc1b014d", "c9f31b827d835692", "14ccd1ad54c810ec"];
    for (finding, key) in findings.iter().zip(keys) {
        let source = runtime
            .add_task(TaskAddParams {
                title: "Legacy compiler observation".to_string(),
                description: format!(
                    "run `{}`\nfailed job (id `{}`)\ncommit actually checked out: `{CHECKOUT}`",
                    finding["run_id"], finding["job_id"]
                ),
                tags: vec![format!("ci-failure:{key}")],
                ..TaskAddParams::default()
            })
            .expect("legacy observation");
        runtime
            .update_task(
                &source.id,
                TaskUpdateParams {
                    status: Some(TaskStatus::Rejected),
                    comment: Some(format!("Duplicate of {}", owner.id)),
                    ..TaskUpdateParams::default()
                },
            )
            .expect("rejected duplicate");
    }
    for finding in &findings {
        let output = file(
            &runtime,
            json!({"ci_evidence": snapshot(vec![finding.clone()])}),
        );
        assert_eq!(output["filed_count"], 0, "{output}");
        assert_eq!(output["skipped_existing"][0]["task_id"], owner.id);
        assert_eq!(
            output["skipped_existing"][0]["match_kind"],
            "confirmed_duplicate"
        );
    }
    let mut reversed = findings.clone();
    reversed.reverse();
    for current in [findings.clone(), reversed] {
        let output = file(&runtime, json!({"ci_evidence": snapshot(current)}));
        assert_eq!(output["filed_count"], 0);
        assert_eq!(output["skipped_existing"][0]["task_id"], owner.id);
        assert_eq!(
            output["skipped_existing"][0]["sources"]
                .as_array()
                .expect("sources")
                .len(),
            3
        );
    }
    // The old chatter key recurs, but a new run is not the original evidence.
    let mut later = findings[0].clone();
    later["run_id"] = json!(99);
    let output = file(&runtime, json!({"ci_evidence": snapshot(vec![later])}));
    assert_eq!(output["filed_count"], 1);
}

#[test]
fn compiler_proof_preserves_case_coordinates_checkout_and_secondary_errors() {
    for difference in [
        "case",
        "location",
        "checkout",
        "secondary",
        "missing_location",
    ] {
        let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
        let mut findings = compiler_findings();
        findings.truncate(2);
        let text = findings[1]["diagnostic_unit"]["text"]
            .as_str()
            .expect("log");
        let changed = match difference {
            "case" => text.replace("owner_machine_id", "Owner_machine_id"),
            "location" => text.replace(":275:13", ":276:13"),
            "checkout" => text.to_string(),
            "secondary" => format!(
                "{text}\nerror[E0308]: mismatched types\n --> crates/orbit-core/src/ci_sweep.rs:275:13\n"
            ),
            _ => text.replace("  --> crates/orbit-core/src/ci_sweep.rs:275:13\n", ""),
        };
        findings[1]["diagnostic_unit"]["text"] = json!(changed);
        if difference == "checkout" {
            findings[1]["actual_checkout_shas"] = json!([NEXT_HEAD]);
        }
        let output = file(&runtime, json!({"ci_evidence": snapshot(findings)}));
        assert_eq!(output["filed_count"], 2, "{difference}: {output}");
    }
}

fn region_finding() -> Value {
    let raw = format!(
        "##[group]Run cargo nextest run\n{}\
         thread 'first_failure' panicked at tests/golden.rs:12:5:\n\
         assertion failed: tool_list.plain.txt golden drift\nleft: {}\nright: expected\n\
         thread 'second_failure' panicked at tests/other.rs:20:7:\n\
         assertion failed: second condition\n\
         ##[error]Process completed with exit code 100.\n",
        "PASS ordinary_test\n".repeat(20_000),
        "large assertion ".repeat(10_000)
    );
    let mut collector = orbit_tools::github_cli::StreamedLogCollector::new(128, 40);
    for chunk in raw.as_bytes().chunks(4096) {
        collector.push(chunk);
    }
    let log = collector.finish();
    let mut finding = failure(10, "CI", "Check", "Run tests", &log.text, CHECKOUT);
    let mut unit = log.failure_regions.expect("selected regions");
    unit["job_id"] = finding["job_id"].clone();
    unit["step"] = json!("Run tests");
    finding["diagnostic_unit"] = unit;
    finding["log_source_complete"] = json!(log.source_complete);
    finding["log_truncated"] = json!(log.truncated);
    finding
}

#[test]
fn oversized_regions_file_all_failures_and_explicit_omissions_then_dedupe() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let evidence = snapshot(vec![region_finding()]);
    let first = file(&runtime, json!({"ci_evidence": evidence}));
    assert_eq!(first["filed_count"], 1);
    let id = filed_task_ids(&first).remove(0);
    let task = runtime.get_task(&id).expect("task");
    for expected in [
        "first_failure",
        "second_failure",
        "golden.rs:12:5",
        "other.rs:20:7",
        "golden drift",
        "full command was not retained",
        "assertion payload bytes",
        "right: expected",
    ] {
        assert!(
            task.description.contains(expected),
            "missing {expected}: {}",
            task.description
        );
    }
    assert!(task.required_tools.is_empty());
    let second = file(&runtime, json!({"ci_evidence": evidence}));
    assert_eq!(second["filed_count"], 0);
    assert_eq!(second["skipped_existing"][0]["task_id"], id);
}

#[test]
fn failure_regions_reject_false_completeness_missing_accounting_and_foreign_identity() {
    for fault in [
        "complete",
        "command",
        "selection",
        "source",
        "job",
        "untruncated_foreign",
        "checkout",
        "step",
        "accounting",
        "assertions",
        "size",
    ] {
        let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
        let mut finding = region_finding();
        match fault {
            "complete" => finding["diagnostic_unit"]["complete"] = json!(true),
            "command" => finding["diagnostic_unit"]["command_complete"] = json!(false),
            "selection" => finding["diagnostic_unit"]["selection_complete"] = json!(false),
            "source" => finding["log_source_complete"] = json!(false),
            "job" => finding["diagnostic_unit"]["job_id"] = json!(920),
            "untruncated_foreign" => {
                finding["diagnostic_unit"]["job_id"] = json!(920);
                finding["log_truncated"] = json!(false);
            }
            "checkout" => finding["checkout_identity"]["provenance"]["job_id"] = json!(920),
            "step" => finding["diagnostic_unit"]["step"] = json!("Other"),
            "accounting" => finding["diagnostic_unit"]["omitted_bytes"] = json!(0),
            "assertions" => {
                finding["diagnostic_unit"]["assertion_payload_omitted_bytes"] = Value::Null
            }
            _ => finding["diagnostic_unit"]["returned_bytes"] = json!(1),
        }
        let error = file_error(&runtime, json!({"ci_evidence": snapshot(vec![finding])}));
        assert!(error.contains("job_evidence_identity"), "{fault}: {error}");
    }
}

/// Consumes the exact production snapshot exported by the engine replay.
/// Both filings use only this disposable runtime's registry and task store.
#[test]
#[ignore = "requires ORBIT_CI_REPLAY_OUTPUT and ORBIT_CI_REPLAY_REPORT"]
fn replay_exact_guardrail_snapshot_through_disposable_filing() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let path = std::env::var("ORBIT_CI_REPLAY_OUTPUT").expect("collection snapshot path");
    let evidence: Value =
        serde_json::from_slice(&std::fs::read(path).expect("snapshot")).expect("JSON");
    let first = file(&runtime, json!({"ci_evidence": evidence}));
    assert_eq!(first["filed_count"], 1, "{first}");
    let id = filed_task_ids(&first).remove(0);
    let task = runtime.get_task(&id).expect("repair task");
    for expected in [
        "101876457414",
        "34165795036",
        "Check / Clippy / Test",
        "Run CI guardrails",
        "3ffa0fd3aaa535867c9e060c7ec600a33d7f2be6",
        "plain_and_json_forms_match_their_goldens",
        "output_goldens.rs:321:5",
        "tool_list.plain.txt",
        "full command was not retained",
        "assertion payload bytes",
    ] {
        assert!(
            task.description.contains(expected),
            "missing {expected}: {}",
            task.description
        );
    }
    assert!(
        !task
            .description
            .contains("eb26940c037ce255b6c28c0378c9276ab38cc75e")
    );
    assert!(task.required_tools.is_empty());
    let second = file(&runtime, json!({"ci_evidence": evidence}));
    assert_eq!(second["filed_count"], 0);
    assert_eq!(second["skipped_existing"][0]["task_id"], id);
    let report = json!({"first_filing": first, "repeat_filing": second, "offline_task_description": task.description, "required_tools": task.required_tools});
    std::fs::write(
        std::env::var("ORBIT_CI_REPLAY_REPORT").expect("report path"),
        serde_json::to_vec_pretty(&report).expect("report JSON"),
    )
    .expect("write replay report");
}
