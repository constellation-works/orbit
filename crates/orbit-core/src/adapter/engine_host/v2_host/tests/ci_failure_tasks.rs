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
const CHECKOUT: &str = "3333333333333333333333333333333333333333";
const NEXT_HEAD: &str = "4444444444444444444444444444444444444444";

fn file(runtime: &OrbitRuntime, input: Value) -> Value {
    runtime
        .run_deterministic(
            "file_ci_failure_tasks",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .expect("file ci failure tasks")
}

fn file_error(runtime: &OrbitRuntime, input: Value) -> String {
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
        "schema_version": 1,
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
            "schema_version": 1,
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
    let log = "ci\tbuild\t2026-08-30T01:00:00Z error: expected 3 arguments, found 2\n";
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

    let second = file(&runtime, json!({"ci_evidence": evidence}));

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
