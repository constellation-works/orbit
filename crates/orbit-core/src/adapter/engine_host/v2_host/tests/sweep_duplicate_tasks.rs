//! Cross-pipeline tests for the shared duplicate-task assessment contract.

use std::cell::Cell;

use orbit_common::OrbitError;
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::{Task, TaskComment, TaskPriority, TaskStatus, TaskType};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::ci_failure_tasks::file_ci_failure_tasks_with_lookup;
use crate::adapter::engine_host::v2_host::dependabot_alert_tasks::file_dependabot_alert_tasks_with_lookup;
use crate::adapter::engine_host::v2_host::duplicate_tasks::DuplicateTaskLookup;
use crate::adapter::engine_host::v2_host::test_support::runtime_with_workspace_layout;
use crate::application::task::{TaskAddParams, TaskUpdateParams};

use super::ci_failure_tasks::{
    failure, filed_task_ids, orb_11513_style_wrangler_log, snapshot as ci_snapshot,
};
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

    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        self.runtime.get_task(task_id)
    }

    fn get_task_comments(&self, task_id: &str) -> Result<Vec<TaskComment>, OrbitError> {
        self.runtime.get_task_comments(task_id)
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

    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        self.runtime.get_task(task_id)
    }

    fn get_task_comments(&self, task_id: &str) -> Result<Vec<TaskComment>, OrbitError> {
        self.runtime.get_task_comments(task_id)
    }
}

struct FailingCommentsLookup<'a> {
    runtime: &'a OrbitRuntime,
}

impl DuplicateTaskLookup for FailingCommentsLookup<'_> {
    fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        self.runtime.list_tasks_by_tags(tags)
    }

    fn list_tasks(&self) -> Result<Vec<Task>, OrbitError> {
        self.runtime.list_tasks()
    }

    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        self.runtime.get_task(task_id)
    }

    fn get_task_comments(&self, _task_id: &str) -> Result<Vec<TaskComment>, OrbitError> {
        Err(injected_lookup_error())
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
fn ci_run_reference_dedupes_open_and_recently_completed_manual_tasks() {
    for status in [TaskStatus::InProgress, TaskStatus::Done] {
        let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
        let task_id = seed_manual_task(
            &runtime,
            "Investigate the failing CI test",
            "The incident is tracked by GitHub Actions run 10; preserve its evidence.",
            status,
        );

        let output = file_ci(&runtime, ci_evidence());

        assert_eq!(output["filed_count"], json!(0), "{output}");
        assert_eq!(output["skipped_existing"][0]["task_id"], json!(task_id));
        assert_eq!(
            output["skipped_existing"][0]["match_evidence"]["fingerprint"],
            json!("ci_failure_run_id")
        );
    }
}

#[test]
fn ci_run_reference_in_a_manual_comment_dedupes_without_sweep_metadata() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let task_id = seed_manual_task(
        &runtime,
        "Investigate the CI regression",
        "Follow up on the reported failure after the incident review.",
        TaskStatus::InProgress,
    );
    runtime
        .update_task(
            &task_id,
            TaskUpdateParams {
                comment: Some("The owner is tracking Actions run 10.".to_string()),
                ..TaskUpdateParams::default()
            },
        )
        .expect("add manual ownership comment");

    let output = file_ci(&runtime, ci_evidence());

    assert_eq!(output["filed_count"], json!(0), "{output}");
    assert_eq!(output["skipped_existing"][0]["task_id"], json!(task_id));
}

#[test]
fn failures_from_two_jobs_for_one_test_form_one_repair_task() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let log = "ci\tjob\t2026-08-30T01:00:00Z test init_report::requested_mcp_records_none_detected_when_no_providers_exist ... FAILED\n";
    let first = failure(10, "CI", "Linux", "Run tests", log, CHECKOUT);
    let mut second = failure(
        10,
        "CI",
        "macOS",
        "Run tests",
        &format!("{log}assertion failed: platform-specific output\n"),
        CHECKOUT,
    );
    second["job_id"] = json!(9910);
    second["log_job_id"] = json!(9910);
    second["checkout_identity"]["provenance"]["job_id"] = json!(9910);
    second["failed_jobs"][0]["job_id"] = json!(9910);
    second["failed_jobs"][0]["url"] =
        json!("https://github.com/acme/orbit/actions/runs/10/job/9910");

    let output = file_ci(&runtime, ci_snapshot(vec![first, second]));

    assert_eq!(output["filed_count"], json!(1), "{output}");
    let task = runtime
        .get_task(output["filed"][0]["task_id"].as_str().expect("task id"))
        .expect("filed task");
    assert!(task.description.contains("Linux"), "{}", task.description);
    assert!(task.description.contains("macOS"), "{}", task.description);
    assert_eq!(output["filed"][0]["jobs"], json!(["Linux", "macOS"]));
}

#[test]
fn newer_green_push_run_marks_an_older_red_run_already_repaired() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let old = failure(10, "CI", "Linux", "Run tests", CI_LOG, CHECKOUT);
    let mut green = failure(11, "CI", "Linux", "Run tests", "", CHECKOUT);
    green["created_at"] = json!("2026-08-30T02:00:00Z");
    green["conclusion"] = json!("success");
    green["failed_jobs"] = json!([]);

    let mut evidence = ci_snapshot(vec![old.clone()]);
    evidence["latest_runs"] = json!([green]);

    let output = file_ci(&runtime, evidence.clone());

    assert_eq!(output["filed_count"], json!(0), "{output}");
    assert_eq!(output["already_repaired"].as_array().map(Vec::len), Some(1));
    assert_eq!(output["already_repaired"][0]["run_id"], json!(10));
    assert_eq!(output["audit"]["already_repaired_run_ids"], json!([10]));
    assert!(runtime.list_tasks().expect("list tasks").is_empty());

    let mut stale_evidence = ci_snapshot(Vec::new());
    stale_evidence["latest_runs"] = json!([evidence["latest_runs"][0].clone()]);
    stale_evidence["stale_or_superseded"] = json!([old]);
    let stale_output = file_ci(&runtime, stale_evidence);
    assert_eq!(stale_output["filed_count"], json!(0), "{stale_output}");
    assert_eq!(
        stale_output["audit"]["already_repaired_run_ids"],
        json!([10])
    );
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

/// Natural prose from the live ORB-11511 brief: specific Wrangler diagnostic,
/// deploy command, commit, and config path, without generated CI labels.
fn orb_11511_manual_brief() -> &'static str {
    "The user reports website publication failing on 2026-09-07 for commit \
     a93caa13890764380e184d996fa709b1bcbe278c. \
     cloudflare/wrangler-action@ebbaa1584979971c8614a24965b4405ff95890e0 installs \
     Wrangler 4.129.0 and runs `wrangler pages deploy dist \
     --project-name=orbit-website --branch=main \
     --commit-hash=a93caa13890764380e184d996fa709b1bcbe278c`. It exits 1 during \
     Pages configuration validation: `Missing top-level field \"name\" in \
     configuration file.` No CI run URL was supplied. Local source inspection \
     confirms website/wrangler.toml contains only compatibility_date and \
     pages_build_output_dir."
}

fn website_wrangler_log() -> String {
    orb_11513_style_wrangler_log(
        "Running configuration file validation for Pages",
        "Missing top-level field \"name\" in configuration file.",
    )
}

fn website_wrangler_evidence() -> Value {
    ci_snapshot(vec![failure(
        10,
        "Website",
        "Publish to Cloudflare Pages",
        "Deploy static site",
        &website_wrangler_log(),
        "a93caa13890764380e184d996fa709b1bcbe278c",
    )])
}

#[test]
fn manual_wrangler_brief_covers_the_same_incident_without_generated_labels_or_tag_mutation() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let task_id = seed_manual_task(
        &runtime,
        "Fix website Pages deployment failing on missing Wrangler project name",
        orb_11511_manual_brief(),
        TaskStatus::InProgress,
    );
    let before = runtime.get_task(&task_id).expect("covering task");
    assert!(
        !before
            .description
            .to_ascii_lowercase()
            .contains("failing job")
            && !before
                .description
                .to_ascii_lowercase()
                .contains("failing step"),
        "fixture must stay natural prose"
    );
    assert!(before.tags.is_empty(), "covering task must stay untagged");

    let output = file_ci(&runtime, website_wrangler_evidence());

    assert_eq!(output["filed_count"], json!(0), "{output}");
    assert_eq!(output["skipped_existing"][0]["task_id"], task_id);
    assert_eq!(
        output["skipped_existing"][0]["match_kind"],
        "material_coverage"
    );
    assert_eq!(
        output["skipped_existing"][0]["match_evidence"]["fingerprint"],
        "ci_failure_error_and_command"
    );
    let after = runtime
        .get_task(&task_id)
        .expect("covering task after sweep");
    assert_eq!(after.tags, before.tags);
    assert_eq!(after.description, before.description);
    assert_eq!(after.title, before.title);
}

#[test]
fn shared_workflow_generic_npx_or_same_file_do_not_suppress_unrelated_failures() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    seed_manual_task(
        &runtime,
        "Website workflow failed because npx exited 1",
        "The Website workflow's Publish job failed when npx exited 1. \
         website/wrangler.toml is the Pages config and should be inspected, \
         but this brief does not quote a missing name field or the deploy command.",
        TaskStatus::Backlog,
    );

    let output = file_ci(&runtime, website_wrangler_evidence());

    assert_eq!(output["filed_count"], json!(1), "{output}");
    assert_eq!(output["skipped_existing"], json!([]));
}

#[test]
fn a_done_manual_repair_does_not_hide_a_later_recurrence() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let prior_incident = orb_11511_manual_brief().replace(
        "a93caa13890764380e184d996fa709b1bcbe278c",
        "4444444444444444444444444444444444444444",
    );
    seed_manual_task(
        &runtime,
        "Fix website Pages deployment failing on missing Wrangler project name",
        &prior_incident,
        TaskStatus::Done,
    );

    let output = file_ci(&runtime, website_wrangler_evidence());

    assert_eq!(output["filed_count"], json!(1), "{output}");
    assert_eq!(output["skipped_existing"], json!([]));
}

#[test]
fn rejected_exact_key_comment_reuses_one_open_covering_owner() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let owner_id = seed_manual_task(
        &runtime,
        "Fix website Pages deployment failing on missing Wrangler project name",
        "Repair the Pages project name. This brief deliberately omits the \
         Wrangler diagnostic and deploy command so coverage must come from \
         the rejected duplicate comment.",
        TaskStatus::InProgress,
    );
    let first = file_ci(&runtime, website_wrangler_evidence());
    assert_eq!(first["filed_count"], json!(1), "{first}");
    let filed_id = filed_task_ids(&first).remove(0);
    runtime
        .update_task(
            &filed_id,
            TaskUpdateParams {
                status: Some(TaskStatus::Rejected),
                comment: Some(format!(
                    "Duplicate of active {owner_id}: both cite the same Website Pages failure."
                )),
                ..TaskUpdateParams::default()
            },
        )
        .expect("reject as duplicate");

    let output = file_ci(&runtime, website_wrangler_evidence());

    assert_eq!(output["filed_count"], json!(0), "{output}");
    assert_eq!(output["skipped_existing"][0]["task_id"], owner_id);
    assert_eq!(
        output["skipped_existing"][0]["match_kind"],
        "confirmed_duplicate"
    );
    assert_eq!(
        output["skipped_existing"][0]["match_evidence"]["fingerprint"],
        "rejected_duplicate_comment"
    );
    let owner = runtime.get_task(&owner_id).expect("covering owner");
    assert!(
        owner.tags.is_empty(),
        "confirmed duplicate must not tag the owner"
    );
}

#[test]
fn rejected_exact_key_without_covering_owner_or_with_done_owner_stays_visible() {
    for (comment, owner_status) in [
        (
            Some("Won't fix; this is an infrastructure flake.".to_string()),
            None,
        ),
        (
            Some("Duplicate of active {owner}: preserve the rejected evidence.".to_string()),
            Some(TaskStatus::Done),
        ),
    ] {
        let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
        let comment = match owner_status {
            Some(status) => {
                let owner_id = seed_manual_task(
                    &runtime,
                    "Fix website Pages deployment failing on missing Wrangler project name",
                    "Repair the Pages project name without quoting the diagnostic.",
                    status,
                );
                comment.map(|template| template.replace("{owner}", &owner_id))
            }
            None => comment,
        };
        let first = file_ci(&runtime, website_wrangler_evidence());
        assert_eq!(first["filed_count"], json!(1), "{first}");
        let filed_id = filed_task_ids(&first).remove(0);
        runtime
            .update_task(
                &filed_id,
                TaskUpdateParams {
                    status: Some(TaskStatus::Rejected),
                    comment,
                    ..TaskUpdateParams::default()
                },
            )
            .expect("reject filed task");

        let output = file_ci(&runtime, website_wrangler_evidence());
        assert_eq!(
            output["filed_count"],
            json!(1),
            "closed or unlinked rejected history must not suppress: {output}"
        );
    }
}

#[test]
fn confirmed_duplicate_comment_lookup_failure_is_redacted_retryable_and_writes_nothing() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let first = file_ci(&runtime, website_wrangler_evidence());
    let filed_id = filed_task_ids(&first).remove(0);
    runtime
        .update_task(
            &filed_id,
            TaskUpdateParams {
                status: Some(TaskStatus::Rejected),
                comment: Some(
                    "Duplicate of active ORB-11511: retain provenance until lookup works."
                        .to_string(),
                ),
                ..TaskUpdateParams::default()
            },
        )
        .expect("reject filed task");
    let before = runtime.list_tasks().expect("list tasks").len();

    let error = file_ci_failure_tasks_with_lookup(
        &runtime,
        &json!({"ci_evidence": website_wrangler_evidence()}),
        &FailingCommentsLookup { runtime: &runtime },
    )
    .expect_err("comment lookup failure must fail closed")
    .to_string();

    assert_retryable_redacted_lookup_error(&error);
    assert_eq!(runtime.list_tasks().expect("list tasks").len(), before);
}
