//! Task commit boundary contracts: staged rename delivery and refusal of
//! unknown fields for no-diff and already-landed evidence [ORB-13881], and
//! refusal of nested repositories that would deliver as gitlinks [ORB-15265],
//! and an implementer's invalid-finding report rejecting its task [ORB-19635].

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::execute_deterministic_action;
use orbit_types::task::{
    ExternalRef, Task, TaskArtifact, TaskComment, TaskPriority, TaskStatus, TaskType,
};
use serde_json::{Value, json};
use tempfile::tempdir;

const TASK_ID: &str = "T1";
const RUN_ID: &str = "jrun-verifier-test";

#[path = "../support/commit_evidence.rs"]
mod evidence_fixture;
use evidence_fixture::VerifierHost;

mod already_landed;
mod finding_invalid;

fn fixture_task() -> Task {
    let now = Utc::now();
    Task {
        granted_tools: Vec::new(),
        requires_labels: Vec::new(),
        job_run_machine: None,
        id: TASK_ID.to_string(),
        title: "Test Task".to_string(),
        description: String::new(),
        acceptance_criteria: vec!["Criterion 1".to_string()],
        tags: Vec::new(),
        required_tools: Vec::new(),
        plan: String::new(),
        execution_summary: "Outcome: success\n\nNo change needed.".to_string(),
        context_files: vec!["file:README.md".to_string()],
        created_by: None,
        planned_by: None,
        implemented_by: None,
        status: TaskStatus::InProgress,
        priority: TaskPriority::Medium,
        complexity: None,
        task_type: TaskType::Bug,
        pr_status: None,
        external_refs: Vec::new(),
        relations: Vec::new(),
        job_run_id: Some(RUN_ID.to_string()),
        crew: None,
        crew_source: None,
        orchestrator: None,
        orchestrator_model: None,
        orchestrator_model_source: None,
        created_by_model: None,
        origin: None,
        rule_fingerprint: None,
        created_at: now,
        updated_at: now,
        not_before: None,
    }
}

fn git_output(dir: &Path, args: &[&str]) -> String {
    let mut command = crate::git_test_support::command();
    let output = command
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git execution");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn init_git_repo(dir: &Path) {
    let git = |args: &[&str]| git_output(dir, args);
    git(&["init"]);
    git(&["config", "user.name", "Test User"]);
    git(&["config", "user.email", "test@example.invalid"]);
    let hooks = dir.join(".git").join("orbit-test-empty-hooks");
    fs::create_dir_all(&hooks).expect("create empty hooks dir");
    git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
    fs::write(dir.join("README.md"), "base\n").expect("write README");
    git(&["add", "README.md"]);
    git(&["commit", "-m", "initial commit [T1]"]);
}

fn temp_repo() -> tempfile::TempDir {
    let temp = tempdir().expect("create tempdir");
    init_git_repo(temp.path());
    temp
}

fn verifier_fixture(task: Task) -> (tempfile::TempDir, VerifierHost, String) {
    let temp = temp_repo();
    let head = git_head(temp.path());
    let host = VerifierHost::new(temp.path(), task);
    (temp, host, head)
}

fn git_head(dir: &Path) -> String {
    git_output(dir, &["rev-parse", "HEAD"])
}

fn action(host: &VerifierHost, input: &Value) -> Result<Value, OrbitError> {
    execute_deterministic_action(
        host,
        "git_commit",
        &json!({}),
        input,
        false,
        &HashMap::new(),
        None,
    )
}

fn per_task_input(repo: &Path, task_id: &str) -> Value {
    json!({
        "scope": "per_task", "job_run_id": RUN_ID,
        "workspace_path": repo, "completed_task_ids": [task_id],
    })
}

fn commit_input(repo: &Path, head: &str) -> Value {
    json!({
        "scope": "all",
        "job_run_id": RUN_ID,
        "workspace_path": repo.to_str().unwrap(),
        "base_sha": head,
        "verify_already_landed": true,
    })
}

fn artifact(path: &str, content: Value) -> TaskArtifact {
    TaskArtifact::from_text(path, serde_json::to_string_pretty(&content).unwrap())
}

fn per_task_commit_delivers_both_sides_of_a_staged_rename() {
    let temp = temp_repo();
    let repo = temp.path();
    git_output(repo, &["config", "diff.renames", "true"]);
    git_output(repo, &["mv", "README.md", "renamed.md"]);
    let mut task = fixture_task();
    task.context_files.push("file:renamed.md".to_string());
    let host = VerifierHost::new(repo, task);

    let result = action(&host, &per_task_input(repo, TASK_ID)).expect("deliver staged rename");

    assert_eq!(result["committed_task_ids"], json!([TASK_ID]));
    assert_eq!(
        git_output(
            repo,
            &[
                "diff",
                "--no-renames",
                "--name-status",
                "HEAD~1",
                "HEAD",
                "--"
            ],
        ),
        "D\tREADME.md\nA\trenamed.md",
        "ORB-14097: the task commit must include the rename source deletion and destination addition"
    );
    assert_eq!(git_output(repo, &["show", "HEAD:renamed.md"]), "base");
    assert!(
        git_output(repo, &["status", "--porcelain", "--untracked-files=all"]).is_empty(),
        "ORB-14097: no staged deletion may leak into a later task's commit"
    );
}

/// A long non-title placeholder must not cut the `[{{task_id}}]` marker away:
/// the templated commit keeps the complete marker within the subject budget
/// and is accepted as already-landed evidence for its own delivery.
fn templated_commit_with_long_non_title_placeholder_preserves_marker_and_lands() {
    use orbit_types::workflow::{ConventionTemplate, DeliveryConventions};

    let long_title = format!("Fix {}", "the very long widget ".repeat(8))
        .trim_end()
        .to_string();
    for (case, title, reference, template, edit) in [
        (
            "long external reference",
            "Fix the widget",
            Some("X".repeat(80)),
            "{{type}}({{ext.jira}}): {{title}} [{{task_id}}]",
            "widget fix\n",
        ),
        (
            "long title",
            long_title.as_str(),
            None,
            "{{type}}: {{title}} [{{task_id}}]",
            "long title fix\n",
        ),
    ] {
        let temp = temp_repo();
        let repo = temp.path();
        let mut task = fixture_task();
        task.title = title.to_string();
        task.external_refs = reference
            .into_iter()
            .map(|id| ExternalRef {
                system: "jira".to_string(),
                id,
                url: None,
            })
            .collect();
        let host = VerifierHost::new(repo, task.clone());
        host.set_conventions(DeliveryConventions {
            commit_subject: Some(
                ConventionTemplate::parse_commit_subject("commit.subject_template", template)
                    .expect("subject template parses"),
            ),
            ..DeliveryConventions::default()
        });
        fs::write(repo.join("README.md"), edit).expect("write README");
        let result = action(&host, &per_task_input(repo, TASK_ID))
            .unwrap_or_else(|error| panic!("{case}: deliver templated commit: {error}"));
        assert_eq!(result["committed_task_ids"], json!([TASK_ID]), "{case}");
        let subject = git_output(repo, &["log", "-1", "--format=%s"]);
        assert!(
            subject.chars().count() <= 72,
            "{case}: subject must stay within the budget, got {subject:?}"
        );
        assert!(
            subject.contains(&format!("[{TASK_ID}]")),
            "{case}: subject must preserve the complete delivery marker, got {subject:?}"
        );
        let message = git_output(repo, &["log", "-1", "--format=%B"]);
        assert!(
            message.contains(title),
            "{case}: shortened subject must keep the full title in the body, got {message:?}"
        );

        if case == "long external reference" {
            // The marker-preserving commit verifies as evidence of its own delivery.
            let head = git_head(repo);
            let mut report = evidence_fixture::verifier_already_landed_report(&head);
            report["scope"] = orbit_types::workflow::handoff::already_landed_scope(&task, &[]);
            host.set_artifacts(
                TASK_ID,
                evidence_artifacts("already-landed.json", report, &head, "make test"),
            );
            let verified = action(&host, &commit_input(repo, &head))
                .expect("a marker-preserving templated commit must verify as already landed");
            assert_eq!(verified["decision"], "verified_already_landed");
            assert_eq!(verified["committed"], false);
        }
    }
}

/// The marker suffix at and next to the one-character remaining budget must
/// not overflow when truncation adds an ellipsis and optional separator.
fn templated_commit_with_near_budget_marker_suffix_preserves_budget() {
    use orbit_types::workflow::{ConventionTemplate, DeliveryConventions};

    for ref_len in [59, 60, 61] {
        let temp = temp_repo();
        let repo = temp.path();
        let mut task = fixture_task();
        task.id = "ORB-18018".to_string();
        task.external_refs = vec![ExternalRef {
            system: "jira".to_string(),
            id: "X".repeat(ref_len),
            url: None,
        }];
        let marker = format!("[{}]", task.id);
        let task_id = task.id.clone();
        let host = VerifierHost::new(repo, task);
        host.set_conventions(DeliveryConventions {
            commit_subject: Some(
                ConventionTemplate::parse_commit_subject(
                    "commit.subject_template",
                    "{{type}}: [{{task_id}}]{{ext.jira}}",
                )
                .expect("subject template parses"),
            ),
            ..DeliveryConventions::default()
        });
        fs::write(repo.join("README.md"), "marker suffix fix\n").expect("write README");
        let result = action(&host, &per_task_input(repo, &task_id))
            .expect("deliver commit with a near-budget marker suffix");
        assert_eq!(result["committed_task_ids"], json!([task_id]));
        let subject = git_output(repo, &["log", "-1", "--format=%s"]);
        assert!(
            subject.chars().count() <= 72,
            "ref length {ref_len} must keep the subject within budget, got {subject:?}"
        );
        assert!(
            subject.contains(&marker),
            "ref length {ref_len} must preserve the complete marker, got {subject:?}"
        );
    }
}

fn commit_verifier_refuses_no_diff_with_covering_commit_or_unknown_fields() {
    let (temp, host, head) = verifier_fixture(fixture_task());

    // `covering_commit` reproduces ORB-13778 / ORB-13848; other extras also refuse.
    for (field, value) in [
        ("covering_commit", json!(head)),
        ("unexpected_field", json!("disallowed")),
    ] {
        let mut report =
            evidence_fixture::verifier_no_diff_report(&head, "make test", "Genuine no-op.");
        report[field] = value;
        host.set_artifacts(
            TASK_ID,
            evidence_artifacts("no-diff.json", report, &head, "make test"),
        );
        let message = action(&host, &commit_input(temp.path(), &head))
            .expect_err(&format!("no-diff.json carrying {field} must be refused"))
            .to_string();
        assert!(
            message.contains(&format!("unknown field `{field}`")),
            "expected refusal mentioning unknown field {field}, got: {message}"
        );
    }
}

fn commit_verifier_refuses_already_landed_with_unknown_fields() {
    let (temp, host, head) = verifier_fixture(fixture_task());

    let mut report = evidence_fixture::verifier_already_landed_report(&head);
    report["unexpected_field"] = json!("disallowed");

    host.set_artifacts(
        TASK_ID,
        evidence_artifacts("already-landed.json", report, &head, "make test"),
    );

    let error = action(&host, &commit_input(temp.path(), &head))
        .expect_err("already-landed.json carrying unknown field must be refused by the verifier");
    let message = error.to_string();
    assert!(
        message.contains("unknown field `unexpected_field`"),
        "expected refusal mentioning unknown field unexpected_field, got: {message}"
    );
}

fn commit_verifier_accepts_valid_no_diff_evidence() {
    let (temp, host, head) = verifier_fixture(fixture_task());

    let report = evidence_fixture::verifier_no_diff_report(&head, "make test", "Genuine no-op.");

    for stale in [None, Some(oversized_artifact("already-landed.json"))] {
        let mut artifacts = evidence_artifacts("no-diff.json", report.clone(), &head, "make test");
        artifacts.extend(stale);
        host.set_artifacts(TASK_ID, artifacts);
        let result = action(&host, &commit_input(temp.path(), &head))
            .expect("valid no-diff evidence must be accepted by the verifier");
        assert_eq!(result["decision"], "verified_no_diff");
        assert_eq!(result["skipped_no_diff_expected"], true);
        assert_eq!(result["committed"], false);
        assert_eq!(
            *host.bulk_reads.lock().unwrap(),
            0,
            "no-diff evidence must use named-only reads"
        );
    }
}

fn evidence_artifacts(path: &str, report: Value, head: &str, command: &str) -> Vec<TaskArtifact> {
    vec![
        artifact(path, report),
        artifact(
            "validation.json",
            evidence_fixture::verifier_validation_log(head, command, 0, "ok"),
        ),
    ]
}

fn valid_already_landed_artifacts(head: &str) -> Vec<TaskArtifact> {
    evidence_artifacts(
        "already-landed.json",
        evidence_fixture::verifier_already_landed_report(head),
        head,
        "make test",
    )
}

fn oversized_artifact(path: &str) -> TaskArtifact {
    TaskArtifact::from_text(path, "x".repeat(1_048_577))
}

fn commit_verifier_accepts_valid_already_landed_evidence() {
    let (temp, host, head) = verifier_fixture(fixture_task());

    // An unused invalid route must never shadow fully bound landing proof.
    for stale in [
        None,
        Some(oversized_artifact("no-diff.json")),
        Some(TaskArtifact::from_text("no-diff.json", "invalid JSON")),
    ] {
        let mut artifacts = valid_already_landed_artifacts(&head);
        artifacts.extend(stale);
        host.set_artifacts(TASK_ID, artifacts);
        let result = action(&host, &commit_input(temp.path(), &head))
            .expect("stale no-diff evidence must not shadow valid landing proof [ORB-16131]");
        assert_eq!(result["decision"], "verified_already_landed");
        assert_eq!(result["skipped_no_diff_expected"], true);
        assert_eq!(result["committed"], false);
        assert_eq!(result["validation_provenance"][0]["tested_head"], head);
        assert_eq!(git_head(temp.path()), head);
        assert!(git_output(temp.path(), &["status", "--porcelain"]).is_empty());
        assert_eq!(
            *host.bulk_reads.lock().unwrap(),
            0,
            "clean-tree evidence must use named-only reads"
        );
    }
}

fn commit_verifier_refuses_required_evidence_and_unknown_reads() {
    let temp = tempdir().expect("create tempdir");
    init_git_repo(temp.path());
    let head = git_head(temp.path());
    let mut input = commit_input(temp.path(), &head);
    // Implementation evidence cannot override invalid reports or unknown reads.
    input["implementation"] = json!({
        "already_resolved": {
            "selectors": ["cargo test -p orbit-engine --test engine commit_verifier::"]
        }
    });
    let valid = valid_already_landed_artifacts(&head);

    for (case, artifacts) in [
        (
            "oversized no-diff without alternate",
            vec![oversized_artifact("no-diff.json")],
        ),
        (
            "invalid no-diff without alternate",
            vec![TaskArtifact::from_text("no-diff.json", "invalid JSON")],
        ),
        (
            "oversized landing report",
            vec![
                oversized_artifact("no-diff.json"),
                oversized_artifact("already-landed.json"),
                valid[1].clone(),
            ],
        ),
        (
            "invalid landing report",
            vec![
                oversized_artifact("no-diff.json"),
                TaskArtifact::from_text("already-landed.json", "invalid JSON"),
                valid[1].clone(),
            ],
        ),
        (
            "oversized required log",
            vec![
                oversized_artifact("no-diff.json"),
                valid[0].clone(),
                oversized_artifact("validation.json"),
            ],
        ),
        (
            "invalid required log",
            vec![
                oversized_artifact("no-diff.json"),
                valid[0].clone(),
                TaskArtifact::from_text("validation.json", "invalid JSON"),
            ],
        ),
        (
            "missing required log",
            vec![oversized_artifact("no-diff.json"), valid[0].clone()],
        ),
        (
            "wrong-run required log",
            vec![
                oversized_artifact("no-diff.json"),
                valid[0].clone(),
                artifact("validation.json", {
                    let mut log =
                        evidence_fixture::verifier_validation_log(&head, "make test", 0, "ok");
                    log["run_id"] = json!("another-run");
                    log
                }),
            ],
        ),
    ] {
        let host = VerifierHost::new(temp.path(), fixture_task());
        host.set_artifacts(TASK_ID, artifacts);
        let error = action(&host, &input).expect_err(case).to_string();
        let required_refusal = match case {
            "oversized no-diff without alternate" => "inline read limit",
            "invalid no-diff without alternate" => "invalid no-diff.json",
            "oversized landing report" | "invalid landing report" => "already-landed.json",
            "oversized required log" | "missing required log" => "validation.json",
            "invalid required log" | "wrong-run required log" => "validation log",
            _ => unreachable!("every refusal case identifies its required evidence"),
        };
        assert!(
            error.contains(required_refusal),
            "{case} must refuse its required evidence: {error}"
        );
        assert_eq!(
            git_head(temp.path()),
            head,
            "{case} must not create a commit"
        );
        assert_eq!(
            *host.bulk_reads.lock().unwrap(),
            0,
            "{case} must use named-only reads"
        );
    }

    // Read uncertainty is not evidence invalidity, even with a valid alternate.
    for (route, reads) in [
        (
            "no-diff.json",
            &[
                ("no-diff.json", 1),
                ("no-diff.json", 2),
                ("validation.json", 1),
            ][..],
        ),
        (
            "already-landed.json",
            &[
                ("already-landed.json", 1),
                ("already-landed.json", 2),
                ("already-landed.json", 3),
                ("validation.json", 1),
            ][..],
        ),
    ] {
        for &(path, nth) in reads {
            let host = VerifierHost::new(temp.path(), fixture_task());
            let mut artifacts = valid.clone();
            artifacts.push(if route == "no-diff.json" {
                artifact(
                    route,
                    evidence_fixture::verifier_no_diff_report(
                        &head,
                        "make test",
                        "No change required.",
                    ),
                )
            } else {
                oversized_artifact("no-diff.json")
            });
            host.set_artifacts(TASK_ID, artifacts);
            *host.unknown_artifact_read.lock().unwrap() = Some((path.into(), nth));
            let error = action(&host, &input)
                .expect_err(&format!("{route}: {path} read {nth} must refuse fallback"));
            assert!(
                error
                    .to_string()
                    .contains("owner call outcome unknown for lost-artifact-read"),
                "{route}: {path} read {nth} must refuse an unknown transport outcome: {error}"
            );
            assert_eq!(git_head(temp.path()), head, "{route}: {path} read {nth}");
        }
    }
}

#[cfg(unix)]
fn commit_verifier_accepts_already_landed_through_symlinked_parent_workspace() {
    let temp = tempdir().expect("create tempdir");
    let real_parent = temp.path().join("real");
    fs::create_dir_all(&real_parent).expect("create real parent");
    let repo = real_parent.join("repo");
    fs::create_dir_all(&repo).expect("create repo");
    init_git_repo(&repo);
    let head = git_head(&repo);

    let symlink_parent = temp.path().join("symlink_parent");
    std::os::unix::fs::symlink(&real_parent, &symlink_parent).expect("create symlink");
    let symlinked_repo = symlink_parent.join("repo");

    let host = VerifierHost::new(&symlinked_repo, fixture_task());

    host.set_artifacts(TASK_ID, valid_already_landed_artifacts(&head));

    let result = action(&host, &commit_input(&symlinked_repo, &head))
        .expect("already-landed evidence through symlinked workspace must be accepted");
    assert_eq!(result["decision"], "verified_already_landed");
    assert_eq!(result["skipped_no_diff_expected"], true);
    assert_eq!(result["committed"], false);
}

#[cfg(unix)]
fn commit_verifier_refuses_already_landed_with_escaping_anchor_symlink() {
    let temp = tempdir().expect("create tempdir");
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).expect("create repo");
    let outside = temp.path().join("outside.txt");
    fs::write(&outside, "outside\n").expect("write outside");
    let escaping_link = repo.join("escaping.txt");
    std::os::unix::fs::symlink(&outside, &escaping_link).expect("create symlink");

    let git = |args: &[&str]| git_output(&repo, args);
    git(&["init"]);
    git(&["config", "user.name", "Test User"]);
    git(&["config", "user.email", "test@example.invalid"]);
    let hooks = repo.join(".git").join("orbit-test-empty-hooks");
    fs::create_dir_all(&hooks).expect("create empty hooks dir");
    git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
    fs::write(repo.join("README.md"), "base\n").expect("write README");
    git(&["add", "README.md", "escaping.txt"]);
    git(&["commit", "-m", "initial commit [T1]"]);
    let head = git_head(&repo);

    let mut task = fixture_task();
    task.context_files = vec!["file:escaping.txt".to_string()];
    let host = VerifierHost::new(&repo, task);

    let mut report = evidence_fixture::verifier_already_landed_report(&head);
    report["scope"]["context_files"] = json!(["file:escaping.txt"]);
    host.set_artifacts(
        TASK_ID,
        evidence_artifacts("already-landed.json", report, &head, "make test"),
    );

    let err = action(&host, &commit_input(&repo, &head))
        .expect_err("anchor symlink escaping workspace must be refused");
    assert!(
        err.to_string()
            .contains("scope anchor is outside the tested workspace"),
        "expected outside workspace refusal, got: {err}"
    );
}

/// Already-landed evidence that is valid except for the overrides applied.
fn already_landed_refusal(
    host: &VerifierHost,
    repo: &Path,
    head: &str,
    mutate: impl FnOnce(&mut Value),
) -> String {
    let mut report = evidence_fixture::verifier_already_landed_report(head);
    mutate(&mut report);

    host.set_artifacts(
        TASK_ID,
        evidence_artifacts("already-landed.json", report, head, "make test"),
    );
    action(host, &commit_input(repo, head))
        .expect_err("malformed already-landed evidence must be refused")
        .to_string()
}

fn commit_verifier_refusal_names_already_landed_field_contract() {
    let (temp, host, head) = verifier_fixture(fixture_task());

    let message = already_landed_refusal(&host, temp.path(), &head, |report| {
        report["criteria_evidence"] = json!([{"criterion": "Criterion 1", "evidence": "ok"}]);
    });
    assert!(
        message.contains("invalid already-landed.json")
            && message.contains("criteria_evidence must be an array of non-empty strings"),
        "object-shaped criteria_evidence must be refused with the string-per-criterion contract, got: {message}"
    );

    let message = already_landed_refusal(&host, temp.path(), &head, |report| {
        report["validation"][0]["role"] = json!("acceptance");
    });
    assert!(
        message.contains("invalid already-landed.json")
            && message.contains("required, expected_failure, excluded, superseded"),
        "unknown validation role must be refused with the accepted roles, got: {message}"
    );

    // A role name used as the outcome repeats F2026-10-709: the entry must be
    // refused with the outcome vocabulary and the expected_failure/failed
    // pairing, not just serde's unknown-variant diagnostic.
    let message = already_landed_refusal(&host, temp.path(), &head, |report| {
        report["validation"][0]["outcome"] = json!("expected_failure");
    });
    assert!(
        message.contains("invalid already-landed.json")
            && message.contains("passed, failed, denied, not_run")
            && message.contains("with role expected_failure"),
        "a role name used as the outcome must be refused with the accepted outcomes and the expected_failure/failed pairing, got: {message}"
    );
}

fn commit_verifier_keeps_refusing_unaccepted_already_landed_evidence() {
    let (temp, host, head) = verifier_fixture(fixture_task());

    type Mutation = fn(&mut Value);
    let cases: [(&str, Mutation); 4] = [
        ("criteria_evidence count differs from the criteria", |r| {
            r["criteria_evidence"] = json!(["a", "b"]);
        }),
        ("empty criteria_evidence string", |r| {
            r["criteria_evidence"] = json!(["  "]);
        }),
        ("non-required role for a required command", |r| {
            r["validation"][0]["role"] = json!("superseded");
        }),
        ("non-passed outcome", |r| {
            r["validation"][0]["outcome"] = json!("failed");
        }),
    ];
    for (case, mutate) in cases {
        let message = already_landed_refusal(&host, temp.path(), &head, mutate);
        assert!(
            message.contains("already_landed_unverified"),
            "{case} must stay refused, got: {message}"
        );
    }
}

/// A report copied from `task.show` carries receipt ids on each comment. The
/// scope comparison stays exact, but the refusal must say which key differs
/// and what shape a comment takes, without echoing task content [F2026-10-449].
fn commit_verifier_refusal_names_the_differing_already_landed_scope_key() {
    let (temp, host, head) = verifier_fixture(fixture_task());
    let at = Utc::now();
    host.set_comments(
        TASK_ID,
        vec![TaskComment {
            at,
            by: "task-pilot".into(),
            message: "secret-comment-text".into(),
            operation_id: Some("op-receipt-1".into()),
        }],
    );
    let comment = |extra: Option<(&str, Value)>| {
        let mut comment = json!({"at": at, "by": "task-pilot", "message": "secret-comment-text"});
        if let Some((key, value)) = extra {
            comment[key] = value;
        }
        comment
    };

    let message = already_landed_refusal(&host, temp.path(), &head, |report| {
        report["scope"]["comments"] =
            json!([comment(Some(("operation_id", json!("op-receipt-1"))))]);
    });
    for expected in [
        "already_landed_unverified",
        "scope.comments",
        "`at`, `by` and `message`",
        "operation_id",
    ] {
        assert!(
            message.contains(expected),
            "a comment carrying a receipt id must be refused naming {expected}, got: {message}"
        );
    }
    assert!(
        !message.contains("secret-comment-text") && !message.contains("op-receipt-1"),
        "the refusal must name keys only, never task content, got: {message}"
    );

    let message = already_landed_refusal(&host, temp.path(), &head, |report| {
        report["scope"]["comments"] = json!([{"at": at, "by": "task-pilot", "message": "edited"}]);
    });
    assert!(
        message.contains("scope.comments") && !message.contains("edited"),
        "a changed comment message must stay refused as a comments difference, got: {message}"
    );

    let message = already_landed_refusal(&host, temp.path(), &head, |report| {
        report["scope"]["comments"] = json!([comment(None)]);
        report["scope"]["title"] = json!("Renamed Task");
    });
    assert!(
        message.contains("scope.title") && !message.contains("scope.comments"),
        "a changed title must be refused naming title only, got: {message}"
    );
    assert!(
        !message.contains("Renamed Task"),
        "the refusal must not echo the reported value, got: {message}"
    );

    let accepted = {
        let mut artifacts = valid_already_landed_artifacts(&head);
        let mut report: Value = serde_json::from_slice(&artifacts[0].content).unwrap();
        report["scope"]["comments"] = json!([comment(None)]);
        artifacts[0] = artifact("already-landed.json", report);
        artifacts
    };
    host.set_artifacts(TASK_ID, accepted);
    let result = action(&host, &commit_input(temp.path(), &head))
        .expect("a scope reduced to at, by and message must be accepted");
    assert_eq!(result["decision"], "verified_already_landed");
}

/// Identity, lineage, ancestry and scope-anchor refusals stay in force
/// alongside the scope diagnostic [F2026-10-449].
fn commit_verifier_keeps_refusing_already_landed_identity_and_covering_mismatches() {
    let temp = tempdir().expect("create tempdir");
    init_git_repo(temp.path());
    let marked = git_head(temp.path());
    fs::write(temp.path().join("other.txt"), "x\n").expect("write file");
    git_output(temp.path(), &["add", "other.txt"]);
    git_output(temp.path(), &["commit", "-m", "unmarked follow-up"]);
    let unmarked = git_head(temp.path());
    let default_branch = git_output(temp.path(), &["rev-parse", "--abbrev-ref", "HEAD"]);
    git_output(temp.path(), &["checkout", "-q", "-b", "side", &marked]);
    fs::write(temp.path().join("side.txt"), "y\n").expect("write file");
    git_output(temp.path(), &["add", "side.txt"]);
    git_output(temp.path(), &["commit", "-m", "side branch [T1]"]);
    let side = git_head(temp.path());
    git_output(temp.path(), &["checkout", "-q", &default_branch]);
    let head = unmarked.clone();

    let host = VerifierHost::new(temp.path(), fixture_task());
    for (case, field, value, expected) in [
        (
            "wrong task id",
            "task_id",
            "T2",
            "evidence must identify this task",
        ),
        (
            "missing covering task id",
            "covering_task_id",
            " ",
            "evidence must identify this task",
        ),
        (
            "run outside the retry lineage",
            "run_id",
            "jrun-unrelated",
            "validation belongs to a different run",
        ),
        (
            "tested HEAD other than the owner-observed base",
            "tested_head",
            marked.as_str(),
            "tested HEAD is not the owner-observed base",
        ),
        (
            "covering commit that is not an ancestor",
            "covering_commit",
            side.as_str(),
            "not a verified ancestor",
        ),
        (
            "covering commit without the task marker",
            "covering_commit",
            head.as_str(),
            "no matching task delivery marker",
        ),
    ] {
        let message = already_landed_refusal(&host, temp.path(), &head, |report| {
            report[field] = json!(value);
        });
        assert!(
            message.contains(expected),
            "{case} must stay refused with `{expected}`, got: {message}"
        );
    }

    // A marked ancestor with undeclared scope fails closed on the covering scope.
    let mut task = fixture_task();
    task.context_files = Vec::new();
    let host = VerifierHost::new(temp.path(), task);
    let message = already_landed_refusal(&host, temp.path(), &head, |report| {
        report["covering_commit"] = json!(marked);
        report["scope"]["context_files"] = json!([]);
    });
    assert!(
        message.contains("covering scope is empty or changed since landing"),
        "an empty declared context_files must stay refused, got: {message}"
    );
}

/// A clean tree at the pinned HEAD with neither route artifact must name both
/// routes, so a task that correctly changed nothing is pointed at no-diff.json
/// [F2026-10-169]. The reason keeps its already_landed_unverified prefix.
fn commit_verifier_clean_tree_without_evidence_names_both_routes() {
    let (temp, host, head) = verifier_fixture(fixture_task());

    let message = action(&host, &commit_input(temp.path(), &head))
        .expect_err("a clean tree without evidence must be refused")
        .to_string();
    for expected in [
        "already_landed_unverified:",
        "no-diff.json",
        "already-landed.json",
    ] {
        assert!(
            message.contains(expected),
            "refusal must name {expected} so a no-change task finds its route [F2026-10-169], got: {message}"
        );
    }
}

/// A `no-diff-expected` task skips a clean stage and commits an unexpected
/// diff. The commit is the normal shipment commit; `sync_base` remains the
/// conflict boundary for that diff [ORB-14247].
fn no_diff_expected_commits_an_unexpected_diff_and_skips_a_clean_tree() {
    let clean = tempdir().expect("create tempdir");
    init_git_repo(clean.path());
    let clean_head = git_head(clean.path());
    let mut clean_task = fixture_task();
    clean_task.tags = vec!["no-diff-expected".to_string()];
    let clean_host = VerifierHost::new(clean.path(), clean_task);
    let skipped = action(&clean_host, &commit_input(clean.path(), &clean_head))
        .expect("a clean no-diff-expected tree skips the commit");
    assert_eq!(skipped["decision"], "skipped_no_diff_expected");
    assert_eq!(skipped["committed"], false);
    assert_eq!(skipped["skipped_no_diff_expected"], true);
    assert_eq!(git_head(clean.path()), clean_head);

    let dirty = tempdir().expect("create tempdir");
    init_git_repo(dirty.path());
    let dirty_head = git_head(dirty.path());
    let mut dirty_task = fixture_task();
    dirty_task.tags = vec!["no-diff-expected".to_string()];
    dirty_task.execution_summary =
        "Outcome: success\n\nThe review edited the tree, so delivery commits that diff."
            .to_string();
    let dirty_host = VerifierHost::new(dirty.path(), dirty_task);
    fs::write(dirty.path().join("README.md"), "review edit\n").expect("dirty tree");
    let committed = action(&dirty_host, &commit_input(dirty.path(), &dirty_head))
        .expect("an unexpected diff on a no-diff-expected task commits");
    assert_eq!(committed["decision"], "performed", "{committed}");
    assert_eq!(committed["committed"], true, "{committed}");
    assert_eq!(committed["skipped_no_diff_expected"], false, "{committed}");
    assert_ne!(git_head(dirty.path()), dirty_head);
    assert_eq!(
        fs::read_to_string(dirty.path().join("README.md")).unwrap(),
        "review edit\n"
    );
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn doc_duties_fixture_task() -> Task {
    let mut task = fixture_task();
    task.task_type = TaskType::Chore;
    task.title = "Validate the oldest workspace documentation".to_string();
    task.tags = vec!["doc-duties".to_string(), "docs".to_string()];
    task.context_files = vec!["dir:.".to_string()];
    task.execution_summary =
        "Outcome: success\n\nVerified 6 documents against current evidence; no drift found."
            .to_string();
    task
}

fn pipeline_commit_step_input(pipeline_asset: &str, repo: &Path, head: &str) -> Value {
    let yaml = fs::read_to_string(
        workspace_root().join(format!("crates/orbit-core/assets/jobs/{pipeline_asset}")),
    )
    .unwrap_or_else(|error| panic!("read {pipeline_asset}: {error}"));
    let parsed: Value = serde_yaml::from_str(&yaml)
        .unwrap_or_else(|error| panic!("parse {pipeline_asset}: {error}"));
    let steps = parsed["spec"]["steps"]
        .as_array()
        .or_else(|| parsed["steps"].as_array())
        .unwrap_or_else(|| panic!("{pipeline_asset} must have steps"));
    let commit_step = steps
        .iter()
        .find(|step| step["id"] == "commit")
        .unwrap_or_else(|| panic!("{pipeline_asset} must have a commit step"));
    let default_input = &commit_step["default_input"];
    assert_eq!(
        default_input["verify_already_landed"], true,
        "{pipeline_asset} commit step must declare verify_already_landed: true [ORB-14644]"
    );

    json!({
        "scope": default_input["scope"].as_str().unwrap_or("all"),
        "job_run_id": RUN_ID,
        "workspace_path": repo.to_str().unwrap(),
        "base_ref": "refs/heads/main",
        "base_sha": head,
        "verify_already_landed": default_input["verify_already_landed"].as_bool().unwrap_or(false),
    })
}

/// A clean doc-duties chore (tagged `doc-duties` + `docs`, with `dir:.` context
/// and no `no-diff-expected` tag) succeeds through the local pipeline's commit
/// step when backed by valid no-diff evidence, without manufacturing a diff.
/// Both local and PR ship modes accept the clean tree, while missing evidence
/// remains refused as an empty stage [ORB-14644].
fn clean_doc_duties_task_completes_local_and_pr_pipeline_commit_with_no_diff_evidence() {
    let temp = temp_repo();
    let repo = temp.path();
    let head = git_head(repo);
    let host = VerifierHost::new(repo, doc_duties_fixture_task());

    let report = evidence_fixture::verifier_no_diff_report(
        &head,
        "make ci-fast",
        "Doc-duties batch accurate; no drift detected.",
    );
    host.set_artifacts(
        TASK_ID,
        evidence_artifacts("no-diff.json", report, &head, "make ci-fast"),
    );

    for pipeline in ["task_local_pipeline.yaml", "task_pr_pipeline.yaml"] {
        let input = pipeline_commit_step_input(pipeline, repo, &head);
        let result = action(&host, &input)
            .unwrap_or_else(|error| panic!("{pipeline} clean doc-duties commit: {error}"));
        assert_eq!(result["decision"], "verified_no_diff", "{pipeline}");
        assert_eq!(result["skipped_no_diff_expected"], true, "{pipeline}");
        assert_eq!(result["committed"], false, "{pipeline}");
    }
    let local_input = pipeline_commit_step_input("task_local_pipeline.yaml", repo, &head);

    // The working tree remains clean and HEAD unchanged
    assert_eq!(git_head(repo), head);
    assert!(git_output(repo, &["status", "--porcelain", "--untracked-files=all"]).is_empty());

    // 3. Without no-diff evidence, an empty doc-duties stage is refused (not skipped)
    let empty_host = VerifierHost::new(repo, doc_duties_fixture_task());
    let error = action(&empty_host, &local_input)
        .expect_err("clean doc-duties task without no-diff evidence must be refused");
    assert!(
        error.to_string().contains("nothing to commit"),
        "expected empty stage refusal, got: {error}"
    );

    // 4. When doc-duties corrected drift, the diff is committed
    fs::write(repo.join("README.md"), "drift corrected\n").expect("write drift edit");
    let committed = action(&host, &local_input)
        .expect("doc-duties task with drift correction commits its changes");
    assert_eq!(committed["decision"], "performed");
    assert_eq!(committed["committed"], true);
    assert_eq!(committed["skipped_no_diff_expected"], false);
    assert_ne!(git_head(repo), head);
    assert_eq!(
        fs::read_to_string(repo.join("README.md")).unwrap(),
        "drift corrected\n"
    );
}

/// A claimed task's selectors settle its empty commit stage for host replay
/// [ORB-22407]. Owner selectors refuse even when supplied directly or recovered
/// from the implement checkpoint [ORB-23194]; missing evidence still refuses.
fn owner_empty_candidates_require_reports_and_claimed_selectors_settle() {
    let temp = temp_repo();
    let repo = temp.path();
    let head = git_head(repo);

    let claimed_host = |repo: &Path| {
        let mut host = VerifierHost::new(repo, fixture_task());
        host.worker = Some(orbit_types::tool::WorkerInvocation {
            owner_machine_id: "owner".into(),
            owner_workspace_id: "ws_owner".into(),
            owner_destination: "owner/ws_owner".into(),
            task_id: TASK_ID.into(),
            claim_id: "claim-verifier".into(),
            execution: orbit_types::task::ExecutionLocation {
                machine_id: "follower".into(),
                machine_name: None,
            },
            bound_run_id: RUN_ID.into(),
        });
        host
    };

    for (pipeline_asset, claimed) in [
        ("task_local_pipeline.yaml", false),
        ("task_pr_pipeline.yaml", false),
        ("task_claimed_local_pipeline.yaml", true),
        ("task_claimed_pr_pipeline.yaml", true),
    ] {
        let host = if claimed {
            claimed_host(repo)
        } else {
            VerifierHost::new(repo, fixture_task())
        };
        let mut input = pipeline_commit_step_input(pipeline_asset, repo, &head);
        input["implementation"] = json!({
            "execution_summary": "Outcome: success\n\nThe defect is already fixed on the pinned base; no change is required.",
            "already_resolved": {
                "selectors": ["cargo test -p orbit-engine --test engine commit_verifier::"]
            },
        });
        if claimed {
            let result = action(&host, &input).unwrap_or_else(|error| {
                panic!("{pipeline_asset} empty candidate must settle, got: {error}")
            });
            assert_eq!(result["decision"], "verified_already_resolved", "{result}");
            assert_eq!(result["committed"], false, "{result}");
            assert_eq!(result["skipped_no_diff_expected"], true, "{result}");
            assert_eq!(result["task_id"], TASK_ID, "{result}");
            assert_eq!(result["base_sha"], head, "{result}");
            assert_eq!(
                result["already_resolved_evidence"]["selectors"],
                json!(["cargo test -p orbit-engine --test engine commit_verifier::"]),
                "{result}"
            );
            assert!(
                result["already_resolved_evidence"]["execution_summary"]
                    .as_str()
                    .is_some_and(|summary| summary.contains("already fixed")),
                "{result}"
            );
            assert_eq!(git_head(repo), head);
            assert!(git_output(repo, &["status", "--porcelain"]).is_empty());
        } else {
            let error = action(&host, &input).expect_err("owner selectors must refuse");
            assert!(
                error.to_string().contains("available only to claimed runs"),
                "{error}"
            );
            // Owner pipelines checkpoint the implementer instead of passing
            // its output directly; that path must enforce the same contract.
            let mut state = orbit_types::workflow::PipelineState::new(
                RUN_ID.into(),
                "task_pr_pipeline".into(),
                json!({}),
            );
            state.record_pipeline_output("implement_one", input["implementation"].clone());
            *host.state.lock().unwrap() = Some(state);
            let mut checkpoint_input = input.clone();
            checkpoint_input
                .as_object_mut()
                .unwrap()
                .remove("implementation");
            let checkpoint_error = action(&host, &checkpoint_input)
                .expect_err("checkpointed owner selectors must refuse");
            assert_eq!(checkpoint_error.to_string(), error.to_string());
            *host.state.lock().unwrap() = None;
        }

        if claimed {
            let mut over_limit = input.clone();
            over_limit["implementation"]["already_resolved"]["selectors"] = json!(
                (0..=orbit_types::workflow::handoff::AlreadyResolved::MAX_SELECTORS)
                    .map(|index| format!("cargo test -p orbit-engine --test engine case_{index}"))
                    .collect::<Vec<_>>()
            );
            let error = action(&host, &over_limit)
                .expect_err("excess host replays must refuse at commit admission");
            assert!(error.to_string().contains("at most"), "{error}");
        }

        // A summary alone names no check that could show the base resolves
        // the task. No implementation output also keeps the empty-stage error.
        for implementation in [
            json!({
                "execution_summary": "Outcome: success\n\nVerified the change already exists on the base.",
            }),
            Value::Null,
        ] {
            input["implementation"] = implementation;
            let error = action(&host, &input)
                .expect_err("an empty candidate without selectors must be refused");
            assert!(
                error.to_string().contains("nothing to commit"),
                "{pipeline_asset} expected empty stage refusal, got: {error}"
            );
        }
        assert_eq!(git_head(repo), head);
        assert!(git_output(repo, &["status", "--porcelain", "--untracked-files=all"]).is_empty());
    }
}

/// Plant a nested repository at `sub/` whose own config runs a clean filter
/// that creates `marker`, as a sandboxed agent can inside its worktree.
fn plant_filtering_nested_repo(repo: &Path, marker: &Path) -> PathBuf {
    let sub = repo.join("sub");
    fs::create_dir_all(&sub).expect("create nested repo");
    let git = |args: &[&str]| git_output(&sub, args);
    git(&["init", "-q"]);
    git(&["config", "user.name", "Agent"]);
    git(&["config", "user.email", "agent@example.invalid"]);
    let hooks = sub.join(".git").join("orbit-test-empty-hooks");
    fs::create_dir_all(&hooks).expect("create empty hooks dir");
    git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
    fs::write(sub.join("f.txt"), "hi\n").expect("write nested file");
    fs::write(sub.join(".gitattributes"), "* filter=pwn\n").expect("write attributes");
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "nested"]);
    git(&[
        "config",
        "filter.pwn.clean",
        &format!("touch '{}' && cat", marker.display()),
    ]);
    make_nested_stat_stale(&sub);
    sub
}

/// Give the nested file a stat its index does not record, so any status
/// inside the nested repository re-hashes it through the clean filter.
fn make_nested_stat_stale(sub: &Path) {
    fs::File::options()
        .write(true)
        .open(sub.join("f.txt"))
        .expect("open nested file")
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(978_307_200))
        .expect("set nested mtime");
}

/// ORB-15265: owner delivery refuses an agent-planted nested repository,
/// which `git add` would commit as a gitlink, and leaves the index unchanged.
/// With a gitlink already in history, delivery's host status and diff never
/// recurse into it, so the nested repository's filter never runs on the host.
fn owner_delivery_refuses_nested_repos_and_never_runs_their_filters() {
    let temp = temp_repo();
    let repo = temp.path();
    let outside = tempdir().expect("create marker dir");
    let marker = outside.path().join("MARKER");
    let mut task = fixture_task();
    task.execution_summary = "Outcome: success\n\nEdited the readme.".to_string();
    let host = VerifierHost::new(repo, task);
    let sub = plant_filtering_nested_repo(repo, &marker);
    fs::write(repo.join("README.md"), "edited\n").expect("write README");
    let head = git_head(repo);
    let index = git_output(repo, &["ls-files", "--stage"]);
    let input = json!({
        "scope": "all",
        "job_run_id": RUN_ID,
        "workspace_path": repo,
    });

    let error = action(&host, &input)
        .expect_err("a nested repository must not be delivered as a gitlink")
        .to_string();
    assert!(
        error.contains("\"sub/\""),
        "refusal names the path: {error}"
    );
    assert_eq!(git_head(repo), head);
    assert_eq!(
        git_output(repo, &["ls-files", "--stage"]),
        index,
        "the refusal leaves the index unchanged"
    );
    assert!(!marker.exists(), "the nested filter ran during the refusal");

    // A gitlink already in history, as owner delivery committed it before
    // this refusal existed.
    git_output(repo, &["add", "--", "sub"]);
    git_output(repo, &["commit", "-q", "-m", "gitlink", "--", "sub"]);
    assert!(
        git_output(repo, &["ls-tree", "HEAD", "sub"]).starts_with("160000 "),
        "the fixture commits a gitlink"
    );
    make_nested_stat_stale(&sub);
    let delivered = action(&host, &input).expect("the tracked edit delivers");
    assert_eq!(delivered["decision"], "performed");
    assert_eq!(
        git_output(repo, &["show", "--format=", "--name-only", "HEAD"]),
        "README.md"
    );
    assert!(
        !marker.exists(),
        "host Git ran the nested repository's filter"
    );

    // Control: plain status without Orbit's host policy does run the filter,
    // so the assertions above observe a real execution path.
    make_nested_stat_stale(&sub);
    let status = crate::git_test_support::command()
        .args(["status", "--porcelain"])
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .expect("run git status");
    assert!(status.status.success());
    assert!(marker.exists(), "the fixture's filter must be reachable");
}

// Keep scenario names in the bounded, isolated family harness without spelling
// each identifier twice.
macro_rules! commit_contracts {
    ($name:ident: $( $(#[$attr:meta])* $row:ident ),+ $(,)?) => {
        #[test]
        fn $name() {
            crate::git_fixture::isolated(module_path!(), stringify!($name), || {
                crate::git_fixture::run_scenarios(&[
                    $( $(#[$attr])* (stringify!($row), $row as fn()) ),+
                ]);
            });
        }
    };
}

commit_contracts!(commit_evidence_contracts_1:
    per_task_commit_delivers_both_sides_of_a_staged_rename,
    commit_verifier_refuses_no_diff_with_covering_commit_or_unknown_fields,
    commit_verifier_refuses_already_landed_with_unknown_fields,
    commit_verifier_accepts_valid_no_diff_evidence,
    templated_commit_with_long_non_title_placeholder_preserves_marker_and_lands,
    templated_commit_with_near_budget_marker_suffix_preserves_budget,
);

commit_contracts!(commit_evidence_contracts_2:
    commit_verifier_accepts_valid_already_landed_evidence,
    commit_verifier_refuses_required_evidence_and_unknown_reads,
    #[cfg(unix)]
    commit_verifier_accepts_already_landed_through_symlinked_parent_workspace,
    #[cfg(unix)]
    commit_verifier_refuses_already_landed_with_escaping_anchor_symlink,
    commit_verifier_refusal_names_already_landed_field_contract,
);

commit_contracts!(commit_evidence_contracts_3:
    commit_verifier_keeps_refusing_unaccepted_already_landed_evidence,
    commit_verifier_refusal_names_the_differing_already_landed_scope_key,
    commit_verifier_keeps_refusing_already_landed_identity_and_covering_mismatches,
    commit_verifier_clean_tree_without_evidence_names_both_routes,
    no_diff_expected_commits_an_unexpected_diff_and_skips_a_clean_tree,
    clean_doc_duties_task_completes_local_and_pr_pipeline_commit_with_no_diff_evidence,
    owner_empty_candidates_require_reports_and_claimed_selectors_settle,
);

/// The command F2026-11-309 captured under a diagnostic label.
const VISUAL_FIXTURE_CHECK: &str = "node --check scripts/visual-fixture-front.mjs";

const LIST_EVERY_CHECK: &str =
    "list every required workspace/task check and concrete evidence for every acceptance criterion";

fn already_landed_with_checks(
    head: &str,
    required_commands: &[&str],
    checks: &[(&str, &str, &str, &str, i64)],
) -> Vec<TaskArtifact> {
    let mut report = evidence_fixture::verifier_already_landed_report(head);
    report["required_commands"] = json!(required_commands);
    report["validation"] = checks
        .iter()
        .map(|(command, role, outcome, path, _)| {
            json!({
                "command": command, "role": role, "outcome": outcome, "log_artifact": path,
            })
        })
        .collect();
    report["criteria_evidence"] =
        json!(["The covering commit already contains the change, and the captured check passed."]);
    let mut artifacts = vec![artifact("already-landed.json", report)];
    for (command, _, _, path, exit_code) in checks {
        artifacts.push(artifact(
            path,
            evidence_fixture::verifier_validation_log(head, command, *exit_code, "captured output"),
        ));
    }
    artifacts
}

/// F2026-11-309: the commit step refused already-landed evidence whose
/// `required_commands` was empty, even when the workspace required nothing
/// and the passing checks were labelled diagnostic. Requirements now come
/// from the host. A passing log covers a required command under either
/// label, and a missing one is refused by name.
fn commit_verifier_derives_already_landed_requirements_from_the_workspace() {
    let (temp, host, head) = verifier_fixture(fixture_task());
    let input = commit_input(temp.path(), &head);

    host.set_required_commands(&[]);
    host.set_artifacts(
        TASK_ID,
        already_landed_with_checks(
            &head,
            &[],
            &[(VISUAL_FIXTURE_CHECK, "diagnostic", "passed", "node.json", 0)],
        ),
    );
    let accepted = action(&host, &input).expect(
        "empty required_commands must be accepted when the workspace requires nothing [F2026-11-309]",
    );
    assert_eq!(accepted["decision"], "verified_already_landed");
    assert_eq!(accepted["committed"], false);
    assert_eq!(git_head(temp.path()), head);

    for role in ["diagnostic", "required"] {
        host.set_required_commands(&[VISUAL_FIXTURE_CHECK]);
        host.set_artifacts(
            TASK_ID,
            already_landed_with_checks(
                &head,
                &[],
                &[(VISUAL_FIXTURE_CHECK, role, "passed", "node.json", 0)],
            ),
        );
        let accepted = action(&host, &input).unwrap_or_else(|error| {
            panic!("a passed exit-0 `{role}` log must cover the workspace command, got: {error}")
        });
        assert_eq!(accepted["decision"], "verified_already_landed", "{role}");
        assert!(
            !accepted.to_string().contains(LIST_EVERY_CHECK),
            "{role} acceptance must not be the generic list-every-check refusal"
        );
    }

    host.set_required_commands(&[VISUAL_FIXTURE_CHECK, "make test"]);
    host.set_artifacts(
        TASK_ID,
        already_landed_with_checks(
            &head,
            &[],
            &[(VISUAL_FIXTURE_CHECK, "diagnostic", "passed", "node.json", 0)],
        ),
    );
    let message = action(&host, &input)
        .expect_err("a workspace command with no passing evidence must be refused")
        .to_string();
    assert!(
        message.contains("already_landed_unverified")
            && message.contains("missing required command `make test`")
            && !message.contains(LIST_EVERY_CHECK)
            && !message.contains(VISUAL_FIXTURE_CHECK),
        "the refusal must name only the missing command, got: {message}"
    );

    host.set_required_commands(&[VISUAL_FIXTURE_CHECK]);
    host.set_artifacts(
        TASK_ID,
        already_landed_with_checks(
            &head,
            &[],
            &[(VISUAL_FIXTURE_CHECK, "diagnostic", "failed", "node.json", 1)],
        ),
    );
    let message = action(&host, &input)
        .expect_err("a failed diagnostic must not cover a required command")
        .to_string();
    assert!(
        message.contains("already_landed_unverified")
            && message.contains(&format!(
                "missing required command `{VISUAL_FIXTURE_CHECK}`"
            ))
            && !message.contains(LIST_EVERY_CHECK),
        "the refusal must name the missing command, got: {message}"
    );
    assert_eq!(git_head(temp.path()), head);
}

commit_contracts!(commit_evidence_contracts_4:
    owner_delivery_refuses_nested_repos_and_never_runs_their_filters,
    commit_verifier_derives_already_landed_requirements_from_the_workspace,
);
