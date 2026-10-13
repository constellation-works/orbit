#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! End-to-end coverage for the task surface after ORB-10428: direct audited
//! status classification through `update --status`, and `orbit task lint` —
//! which reports context that needs repair and, with `--restore-pruned`,
//! re-declares what an earlier prune recorded ([ORB-12490]). Lock
//! administration lives under `task locks` (`list`/`release`).

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::{Value, json};
use tempfile::tempdir;

/// [ORB-12245] `done` and `archived` are terminal on the governed surface.
/// A human on the bare CLI can still override the table with `--force`, and
/// the override is named in the task's history.
fn update_status_reopens_terminal_tasks_only_with_an_explicit_force(workspace: &TestWorkspace) {
    let id = workspace.add_task("Terminal task");
    workspace.drive_to_done(&id);

    let refused = workspace.run_raw(&["task", "update", &id, "--status", "rejected"]);
    assert!(!refused.status.success(), "done must not reopen silently");
    let refusal = String::from_utf8_lossy(&refused.stderr).to_string();
    assert!(
        refusal.contains("cannot move from 'done' to 'rejected'"),
        "the refusal must name the pair: {refusal}"
    );

    let reopened = workspace.task_json(&[
        "task", "update", &id, "--status", "rejected", "--force", "--json",
    ]);
    assert_eq!(reopened["status"], json!("rejected"));
    assert!(!reopened["execution_summary"].as_str().unwrap().is_empty());

    let forced = reopened["history"]
        .as_array()
        .expect("task history")
        .last()
        .expect("forced event")
        .clone();
    assert_eq!(forced["event"], json!("forced"));
    assert_eq!(forced["from_status"], json!("done"));
    assert_eq!(forced["to_status"], json!("rejected"));

    // Reconsidering a rejection needs no override.
    let reconsidered =
        workspace.task_json(&["task", "update", &id, "--status", "backlog", "--json"]);
    assert_eq!(reconsidered["status"], json!("backlog"));
}

/// `orbit task archive` shelves a task from any status; restoring it is the
/// same human override as reopening a completed one.
fn update_status_restores_an_archived_task_with_force(workspace: &TestWorkspace) {
    let id = workspace.add_task("Restore me");
    workspace.run(&["task", "update", &id, "--status", "backlog"], "approve");
    workspace.run(&["task", "archive", &id], "archive");

    let refused = workspace.run_raw(&["task", "update", &id, "--status", "backlog"]);
    assert!(
        !refused.status.success(),
        "archived must not restore silently"
    );

    let restored = workspace.task_json(&[
        "task", "update", &id, "--status", "backlog", "--force", "--json",
    ]);
    assert_eq!(restored["status"], json!("backlog"));
}

/// Completion evidence is required wherever the status is set: the CLI is no
/// more able to mint a `done` task out of a proposal than an agent is.
fn update_status_refuses_completion_without_review_and_evidence(workspace: &TestWorkspace) {
    let id = workspace.add_task("Fabricated completion");

    let skipped = workspace.run_raw(&["task", "update", &id, "--status", "done"]);
    assert!(!skipped.status.success());
    assert!(
        String::from_utf8_lossy(&skipped.stderr).contains("reachable only from 'review'"),
        "completion must not skip review"
    );

    workspace.run(&["task", "update", &id, "--status", "backlog"], "approve");
    workspace.run(
        &[
            "task",
            "update",
            &id,
            "--plan",
            "1) do it",
            "--status",
            "in-progress",
        ],
        "start",
    );
    workspace.run(&["task", "update", &id, "--status", "review"], "to review");

    let unproven = workspace.run_raw(&["task", "update", &id, "--status", "done"]);
    assert!(!unproven.status.success());
    assert!(
        String::from_utf8_lossy(&unproven.stderr).contains("execution summary"),
        "completion must name its missing evidence"
    );

    let done = workspace.task_json(&[
        "task",
        "update",
        &id,
        "--execution-summary",
        "did it",
        "--status",
        "done",
        "--json",
    ]);
    assert_eq!(done["status"], json!("done"));
}

fn update_status_performs_approve_and_reject_transitions(workspace: &TestWorkspace) {
    let [approve, reject] = workspace.add_tasks(["Approve via update", "Reject via update"]);

    // proposed -> backlog (former `approve`).
    let task = workspace.task_json(&["task", "update", &approve, "--status", "backlog", "--json"]);
    assert_eq!(task["status"], json!("backlog"));

    // proposed -> rejected (former `reject`).
    let task = workspace.task_json(&["task", "update", &reject, "--status", "rejected", "--json"]);
    assert_eq!(task["status"], json!("rejected"));
}

/// [ORB-13985] `--discard-candidate` records the operator's discard in task
/// history, alongside a requeue, so the next run implements fresh; it is
/// refused while a run is still working on the task.
fn update_discard_candidate_records_the_discard_unless_a_run_is_active(workspace: &TestWorkspace) {
    let id = workspace.add_task("Discard a candidate");
    workspace.run(&["task", "update", &id, "--status", "backlog"], "approve");
    workspace.run(
        &[
            "task",
            "update",
            &id,
            "--plan",
            "1) do it",
            "--status",
            "in-progress",
        ],
        "start",
    );

    let refused = workspace.run_raw(&["task", "update", &id, "--discard-candidate"]);
    assert!(
        !refused.status.success(),
        "an active run's candidate is not the operator's to discard"
    );
    let refusal = String::from_utf8_lossy(&refused.stderr).to_string();
    assert!(refusal.contains("in-progress"), "{refusal}");
    let discards = |task: &Value| {
        task["history"]
            .as_array()
            .expect("task history")
            .iter()
            .filter(|entry| entry["event"] == json!("candidate_discarded"))
            .count()
    };
    let current = workspace.task_json(&["task", "show", &id, "--json"]);
    assert_eq!(discards(&current), 0, "a refused discard records nothing");

    workspace.run(&["task", "update", &id, "--status", "blocked"], "block");
    let requeued = workspace.task_json(&[
        "task",
        "update",
        &id,
        "--status",
        "backlog",
        "--discard-candidate",
        "--json",
    ]);
    assert_eq!(requeued["status"], json!("backlog"));
    assert_eq!(discards(&requeued), 1);
}

fn task_update_repeats_dependencies_and_clears_context(workspace: &TestWorkspace) {
    let [dependency_a, dependency_b, id] =
        workspace.add_tasks(["First dependency", "Second dependency", "List update"]);
    fs::write(workspace.work.join("one.rs"), "// one\n").expect("write one file");

    workspace.run(
        &[
            "task",
            "update",
            &id,
            "--dependencies",
            &dependency_a,
            "--dependencies",
            &dependency_b,
            "--context",
            "file:one.rs",
        ],
        "update repeated dependencies and context",
    );

    let dependencies =
        workspace.task_json(&["task", "show", &id, "--fields", "dependencies", "--json"]);
    assert_eq!(dependencies, json!([dependency_a, dependency_b]));

    workspace.run(&["task", "update", &id, "--context", ""], "clear context");
    let context =
        workspace.task_json(&["task", "show", &id, "--fields", "context_files", "--json"]);
    assert_eq!(context, json!([]));
}

fn task_update_rejects_missing_context_file_and_leaves_task_unchanged(workspace: &TestWorkspace) {
    let id = workspace.add_task("Target task");
    let before = workspace.task_json(&["task", "show", &id, "--json"]);

    let output = workspace.run_raw(&["task", "update", &id, "--context", "file:does/not/exist.rs"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("file:does/not/exist.rs"),
        "stderr must name selector: {stderr}"
    );

    let after = workspace.task_json(&["task", "show", &id, "--json"]);
    assert_eq!(
        before, after,
        "task must be unchanged after rejected context update"
    );
}

fn task_update_and_add_accept_valid_context_selectors(workspace: &TestWorkspace) {
    fs::write(
        workspace.work.join("existing.rs"),
        "pub fn my_symbol() {}\n",
    )
    .expect("write existing file");
    fs::create_dir_all(workspace.work.join("existing_dir")).expect("create existing dir");

    let added = workspace.task_json(&[
        "task",
        "add",
        "--title",
        "Add with valid context",
        "--complexity",
        "low",
        "--context",
        "file:existing.rs,dir:existing_dir,symbol:existing.rs#my_symbol:function",
        "--json",
    ]);
    assert_eq!(
        added["context_files"],
        json!([
            "file:existing.rs",
            "dir:existing_dir",
            "symbol:existing.rs#my_symbol:function"
        ])
    );

    let id = added["id"].as_str().unwrap();
    let updated = workspace.task_json(&[
        "task",
        "update",
        id,
        "--context",
        "file:existing.rs",
        "--json",
    ]);
    assert_eq!(updated["context_files"], json!(["file:existing.rs"]));
}

/// [ORB-13753] Scope extensions must send the full union: a tool update with
/// only additions previously dropped the task's original selectors.
fn tool_task_update_context_preserves_omissions_replaces_lists_and_clears(
    workspace: &TestWorkspace,
) {
    let routing = workspace.task_json(&["workspace", "show", "--format", "json"]);
    let work = fs::canonicalize(&workspace.work).expect("canonical fixture work");
    assert_eq!(routing["registered"], json!(true));
    assert_eq!(
        routing["checkout"]["repo_root"],
        json!(work.to_string_lossy())
    );
    assert_eq!(
        routing["checkout"]["orbit_dir"],
        json!(work.join(".orbit").to_string_lossy())
    );

    for name in ["existing.rs", "replacement.rs", "[]"] {
        fs::write(workspace.work.join(name), "pub fn fixture() {}\n").expect("fixture file");
    }
    fs::create_dir(workspace.work.join("existing_dir")).expect("fixture directory");
    let id = workspace.add_task("Context replacement semantics");
    let original = json!(["file:existing.rs", "dir:existing_dir"]);

    for (field, replacement, empty) in [
        ("context_files", json!(["file:replacement.rs"]), json!([])),
        ("context_files", json!("file:replacement.rs"), json!(",")),
        ("context", json!("file:replacement.rs"), json!(",")),
    ] {
        workspace.run(
            &[
                "task",
                "update",
                &id,
                "--context",
                "file:existing.rs,dir:existing_dir",
            ],
            "seed context",
        );
        let omitted =
            json!({"id": id, "model": "codex", "comment": "Preserve context"}).to_string();
        let updated =
            workspace.task_json(&["tool", "run", "orbit.task.update", "--input", &omitted]);
        assert_eq!(updated["context_files"], original);
        assert_eq!(
            workspace.task_json(&["task", "show", &id, "--json"])["context_files"],
            original,
            "omitting both context fields must preserve the durable list"
        );

        let invalid = json!({"id": id, "model": "codex", field: ""}).to_string();
        let refused = workspace.run_raw(&["tool", "run", "orbit.task.update", "--input", &invalid]);
        assert!(
            !refused.status.success(),
            "empty {field} string must be rejected"
        );
        assert_eq!(
            workspace.task_json(&["task", "show", &id, "--json"])["context_files"],
            original,
            "rejection must leave the durable context unchanged"
        );

        for (value, expected) in [
            (replacement, json!(["file:replacement.rs"])),
            (json!("[]"), json!(["file:[]"])),
            (empty, json!([])),
        ] {
            let input = json!({"id": id, "model": "codex", field: value}).to_string();
            let updated =
                workspace.task_json(&["tool", "run", "orbit.task.update", "--input", &input]);
            assert_eq!(
                updated["context_files"], expected,
                "replacement via {field}"
            );
            assert_eq!(
                workspace.task_json(&["task", "show", &id, "--json"])["context_files"],
                expected,
                "supplied {field} must replace the whole durable list, including clearing"
            );
        }
    }
}

/// A proposed or backlog task's readiness gaps reach whoever files or edits
/// it — the `orbit.task.add` / `orbit.task.update` responses — and
/// `orbit task show` prints them as a `Readiness:` line.
fn task_show_and_write_tools_report_readiness_gaps(workspace: &TestWorkspace) {
    fs::write(workspace.work.join("existing.rs"), "pub fn fixture() {}\n").expect("fixture file");
    let codes = |readiness: &Value| -> Vec<(String, String)> {
        readiness["gaps"]
            .as_array()
            .expect("readiness gaps")
            .iter()
            .map(|gap| {
                (
                    gap["code"].as_str().expect("code").to_string(),
                    gap["severity"].as_str().expect("severity").to_string(),
                )
            })
            .collect()
    };

    let add = json!({
        "title": "Readiness over the tool surface",
        "description": "Filed without context selectors.",
        "complexity": "low",
        "workspace": "trimmed-surface-test",
        "model": "codex",
    })
    .to_string();
    let added = workspace.task_json(&["tool", "run", "orbit.task.add", "--input", &add]);
    let id = added["id"].as_str().expect("task id").to_string();
    assert_eq!(added["readiness"]["ready"], json!(false));
    assert_eq!(
        codes(&added["readiness"]),
        [("missing_context_files".to_string(), "blocking".to_string())]
    );

    let shown = workspace.run(&["task", "show", &id], "show unready task");
    let text = String::from_utf8_lossy(&shown.stdout);
    assert!(
        text.contains("Readiness: not ready") && text.contains("missing_context_files [blocking]"),
        "show names the gap:\n{text}"
    );
    assert_eq!(
        workspace.task_json(&["task", "show", &id, "--json"])["readiness"],
        added["readiness"]
    );

    let update =
        json!({"id": id, "model": "codex", "context_files": ["file:existing.rs"]}).to_string();
    let updated = workspace.task_json(&["tool", "run", "orbit.task.update", "--input", &update]);
    assert_eq!(updated["readiness"], json!({"ready": true, "gaps": []}));
    let shown = workspace.run(&["task", "show", &id], "show ready task");
    assert!(String::from_utf8_lossy(&shown.stdout).contains("Readiness: ready"));

    // Readiness describes only work waiting to be approved or admitted.
    workspace.drive_to_done(&id);
    let done = workspace.task_json(&["task", "show", &id, "--json"]);
    assert!(done.get("readiness").is_none(), "{done}");
    let shown = workspace.run(&["task", "show", &id], "show done task");
    assert!(!String::from_utf8_lossy(&shown.stdout).contains("Readiness:"));
}

/// [ORB-15191] A write that leaves a task in the backlog with no
/// `context_files` warns that it holds no file lock, on the CLI and MCP
/// surfaces alike. The warning is the task's own advisory backlog readiness
/// gap with its fix, so the assertion pins that consistency rather than the
/// wording. A task with selectors or tagged `no-diff-expected` gets none.
fn backlog_writes_without_context_warn_about_the_missing_file_lock(workspace: &TestWorkspace) {
    fs::write(workspace.work.join("existing.rs"), "pub fn fixture() {}\n").expect("fixture file");
    let expected = |task: &Value| -> Value {
        let gaps = task["readiness"]["gaps"]
            .as_array()
            .expect("readiness gaps");
        assert_eq!(gaps.len(), 1, "{task}");
        let gap = &gaps[0];
        assert_eq!(gap["code"], json!("missing_context_files"), "{task}");
        assert_eq!(gap["severity"], json!("advisory"), "{task}");
        json!([format!(
            "{} Fix: {}.",
            gap["message"].as_str().expect("gap message"),
            gap["fix"].as_str().expect("gap fix")
        )])
    };
    let add = |extra: &[&str]| -> Value {
        let mut args = vec![
            "task",
            "add",
            "--title",
            "Backlog work",
            "--complexity",
            "low",
            "--status",
            "backlog",
            "--json",
        ];
        args.extend_from_slice(extra);
        workspace.task_json(&args)
    };

    let cold = add(&[]);
    assert_eq!(cold["status"], json!("backlog"), "{cold}");
    assert_eq!(cold["warnings"], expected(&cold), "{cold}");
    for extra in [
        ["--context", "file:existing.rs"],
        ["--tag", "no-diff-expected"],
    ] {
        let added = add(&extra);
        assert!(added.get("warnings").is_none(), "{extra:?}: {added}");
    }

    let id = cold["id"].as_str().expect("task id");
    let updated = workspace.task_json(&["task", "update", id, "--priority", "high", "--json"]);
    assert_eq!(updated["warnings"], expected(&updated), "{updated}");
    let input = json!({"id": id, "model": "codex", "priority": "low"}).to_string();
    let tool = workspace.task_json(&["tool", "run", "orbit.task.update", "--input", &input]);
    assert_eq!(tool["warnings"], expected(&tool), "{tool}");
    let input =
        json!({"id": id, "model": "codex", "context_files": ["file:existing.rs"]}).to_string();
    let prepared = workspace.task_json(&["tool", "run", "orbit.task.update", "--input", &input]);
    assert!(prepared.get("warnings").is_none(), "{prepared}");
}

/// The existence guard is an operator-surface default, not a wall: work that
/// creates a file records its selector with the explicit escape, and that
/// declaration is durable creation intent for exactly the selectors it named.
/// Re-sending a declared target needs no new escape; a typo, a revoked
/// target, a traversal and a wrong-kind target are refused.
fn allow_missing_context_records_durable_exact_creation_intent(workspace: &TestWorkspace) {
    fs::write(workspace.work.join("existing.rs"), "pub fn fixture() {}\n").expect("fixture file");

    let added = workspace.task_json(&[
        "task",
        "add",
        "--title",
        "Create a new module",
        "--complexity",
        "low",
        "--context",
        "file:existing.rs,file:src/future.rs",
        "--allow-missing-context",
        "--json",
    ]);
    assert_eq!(
        added["context_files"],
        json!(["file:existing.rs", "file:src/future.rs"])
    );
    let id = added["id"].as_str().expect("task id").to_string();
    assert_eq!(workspace.creation_grant(&id), json!(["file:src/future.rs"]));

    // An ordinary write keeps the declared target without the escape.
    workspace.run(
        &[
            "task",
            "update",
            &id,
            "--context",
            "file:src/future.rs,file:existing.rs",
        ],
        "strict re-send",
    );
    assert_eq!(workspace.creation_grant(&id), json!(["file:src/future.rs"]));
    let typo = workspace.run_raw(&[
        "task",
        "update",
        &id,
        "--context",
        "file:existing.rs,file:src/futur.rs",
    ]);
    assert!(
        !typo.status.success(),
        "an undeclared missing selector stays refused"
    );

    // The agent tool surface: dropping the target revokes it, and a revoked
    // target is not accepted back without a new declaration.
    let revoke =
        json!({"id": id, "model": "codex", "context_files": ["file:existing.rs"]}).to_string();
    workspace.task_json(&["tool", "run", "orbit.task.update", "--input", &revoke]);
    assert_eq!(workspace.creation_grant(&id), json!([]));
    let revived = json!({
        "id": id, "model": "codex",
        "context_files": ["file:existing.rs", "file:src/future.rs"],
    })
    .to_string();
    let refused = workspace.run_raw(&["tool", "run", "orbit.task.update", "--input", &revived]);
    assert!(!refused.status.success(), "a revoked grant must not revive");

    let reauthorize = json!({
        "id": id, "model": "codex",
        "context_files": ["file:existing.rs", "file:src/future.rs"],
        "allow_missing_context": true,
    })
    .to_string();
    workspace.task_json(&["tool", "run", "orbit.task.update", "--input", &reauthorize]);
    assert_eq!(workspace.creation_grant(&id), json!(["file:src/future.rs"]));

    for (selector, case) in [
        ("file:../outside.rs", "traversal"),
        ("dir:existing.rs", "an existing target of the other kind"),
    ] {
        let refused = workspace.run_raw(&[
            "task",
            "update",
            &id,
            "--context",
            selector,
            "--allow-missing-context",
        ]);
        assert!(!refused.status.success(), "{case} must stay refused");
    }
    assert_eq!(workspace.creation_grant(&id), json!(["file:src/future.rs"]));

    let tool_add = json!({
        "title": "Tool-declared module",
        "description": "orbit.task.add records the same intent",
        "complexity": "low",
        "context_files": ["file:src/tool_future.rs"],
        "allow_missing_context": true,
    })
    .to_string();
    let added = workspace.task_json(&["tool", "run", "orbit.task.add", "--input", &tool_add]);
    let tool_id = added["id"].as_str().expect("tool task id");
    assert_eq!(
        workspace.creation_grant(tool_id),
        json!(["file:src/tool_future.rs"])
    );
}

fn locks_list_projects_files_held_by_active_tasks(workspace: &TestWorkspace) {
    fs::write(workspace.work.join("held.rs"), "// held\n").expect("write held file");

    let id = workspace.add_task("Holds a lock");
    workspace.run(&["task", "update", &id, "--status", "backlog"], "approve");
    workspace.run(
        &[
            "task",
            "update",
            &id,
            "--plan",
            "1) hold the file",
            "--context",
            "file:held.rs",
            "--status",
            "in-progress",
        ],
        "start with context",
    );

    let output = workspace.run(&["task", "locks", "list", "--json"], "task locks list");
    let value: Value = serde_json::from_slice(&output.stdout).expect("locked JSON");
    assert_eq!(value["total_tasks"], json!(1));
    assert_eq!(value["locked_files"], json!(["file:held.rs"]));
    assert_eq!(value["by_task"][0]["id"], json!(id));
    // ORB-10651: the CLI must project the same `by_reservation` /
    // `total_reservations` fields the underlying `orbit.task.locks` tool
    // returns, not a hand-built projection that omits them.
    assert_eq!(value["by_reservation"], json!([]));
    assert_eq!(value["total_reservations"], json!(0));

    let text = workspace.run(&["task", "locks", "list"], "task locks list text");
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(stdout.contains("file:held.rs"), "{stdout}");
}

#[test]
fn task_add_attributes_from_model_flag_and_managed_identity_env() {
    const TEST: &str =
        "task_trimmed_surface::task_add_attributes_from_model_flag_and_managed_identity_env";
    const CHILD: &str = "ORBIT_TASK_ATTRIBUTES_MANAGED_IDENTITY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let home = tempdir().expect("isolated child home");
        let output_dir = tempdir().expect("child output directory");
        let mut command = Command::new(std::env::current_exe().expect("test binary"));
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .current_dir(home.path());
        let output = test_env::run_child_test(&mut command, TEST, output_dir.path());
        test_env::assert_child_test_passed(TEST, output.status, &output.stdout, &output.stderr);
        return;
    }

    let ambient = workspace();
    let workspace = workspace();

    let explicit = workspace.task_json(&[
        "task",
        "add",
        "--title",
        "Explicit model",
        "--description",
        "Model flag attribution",
        "--complexity",
        "low",
        "--model",
        "gpt-6-sol",
        "--json",
    ]);
    assert_eq!(explicit["created_by"], json!("codex"));

    let ambient_registry = ambient.home.join(".orbit");
    let ambient_registry = ambient_registry
        .to_str()
        .expect("ambient registry path is UTF-8");
    let before = ambient.task_json(&["task", "list", "--json"]);
    let _ambient_env = test_env::scoped([
        ("ORBIT_ROOT", Some("/sentinel/orbit-root")),
        ("ORBIT_SESSION_ID", Some("sentinel-session")),
        ("ORBIT_TASK_ID", Some("sentinel-task")),
        ("ORBIT_RUN_ID", Some("sentinel-run")),
        ("ORBIT_ACTIVITY_ID", Some("sentinel-activity")),
        ("ORBIT_STEP_INDEX", Some("sentinel-step")),
        ("ORBIT_AGENT_NAME", Some("sentinel-agent")),
        ("ORBIT_AGENT_MODEL", Some("sentinel-model")),
        ("ORBIT_OPERATOR", Some("1")),
        ("ORBIT_MANAGED_RUN_CONTEXT", Some("1")),
        ("ORBIT_TASK_ACTOR_KIND", Some("sentinel-actor")),
        ("ORBIT_REGISTRY_ROOT", Some(ambient_registry)),
        ("ORBIT_WORKSPACE", Some("trimmed-surface-test")),
    ]);

    for _ in 0..2 {
        let output = run_orbit_with_identity(
            &workspace.work,
            &workspace.home,
            &[
                "task",
                "add",
                "--title",
                "Managed identity",
                "--description",
                "Environment attribution",
                "--complexity",
                "low",
                "--json",
            ],
            "codex",
            "gpt-5.6-terra",
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let managed: Value = serde_json::from_slice(&output.stdout).expect("managed task JSON");
        assert_eq!(managed["created_by"], json!("codex"));
    }

    let after = ambient.task_json(&["task", "list", "--json"]);
    assert_eq!(after, before, "ambient sentinel workspace was modified");

    let fixture_tasks = workspace.task_json(&["task", "list", "--json"]);
    let fixture_tasks = fixture_tasks.as_array().expect("fixture task list");
    assert_eq!(fixture_tasks.len(), 3);
    assert!(
        fixture_tasks
            .iter()
            .all(|task| task["created_by"] == json!("codex")),
        "full model strings normalize to the canonical family"
    );
}

fn locks_release_reaches_the_admin_tool_only_with_the_operator_capability(
    workspace: &TestWorkspace,
) {
    // ORB-10651: reservation ids must have the `reservation-<id>` form or
    // `release` now rejects them before reaching the "no matching row" path
    // this test otherwise exercises.
    const RELEASE: &[&str] = &[
        "task",
        "locks",
        "release",
        "reservation-no-such-reservation",
        "--confirm",
    ];

    let refused = workspace.run_raw(&[
        "task",
        "locks",
        "release",
        "reservation-no-such-reservation",
    ]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("--confirm"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );

    // ORB-10453: `orbit task locks release` reaches an MCP-inactive tool
    // through the admin `runtime.run_tool` path — the bypass this task closed.
    // The tool chokepoint governs that path too, so an unidentified caller is
    // refused there rather than silently executing.
    let ungoverned = workspace.run_raw(RELEASE);
    assert!(!ungoverned.status.success());
    let denial = String::from_utf8_lossy(&ungoverned.stderr);
    assert!(denial.contains("capability denied"), "{denial}");
    assert!(denial.contains("operator or runner"), "{denial}");

    // With the capability claimed, the tool runs its own business logic: an
    // unknown reservation yields a structured `released: false`, NOT the
    // `ensure_tool_agent_facing` rejection.
    let output = workspace.run_as_operator(RELEASE, "task locks release");
    let value: Value = serde_json::from_slice(&output.stdout).expect("release JSON");
    assert_eq!(value["released"], json!(false));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("inactive on the agent tool surface"),
        "locks-release must bypass the agent-surface gate:\n{stderr}"
    );
}

fn audit_prune_refuses_unconfirmed_then_deletes_when_confirmed(workspace: &TestWorkspace) {
    workspace.add_task("Create an audit event");
    let before = workspace.task_json(&["audit", "list", "--limit", "100", "--json"]);
    assert!(!before.as_array().expect("audit rows").is_empty());

    let refused = workspace.run_raw(&["audit", "prune", "--older-than", "0s"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--confirm"));
    let after_refusal = workspace.task_json(&["audit", "list", "--limit", "100", "--json"]);
    assert_eq!(after_refusal, before);

    let confirmed = workspace.run_as_operator(
        &["audit", "prune", "--older-than", "0s", "--confirm"],
        "confirmed audit prune",
    );
    assert!(String::from_utf8_lossy(&confirmed.stdout).contains("Pruned"));
    let after = workspace.task_json(&["audit", "list", "--limit", "100", "--json"]);
    assert!(after.as_array().expect("audit rows").is_empty());
}

use crate::task_workspace::{TaskSeed, TestWorkspace};

fn workspace() -> TestWorkspace {
    let mut template = TaskSeed::new("");
    template.description = "Created by the trimmed-surface integration test.".into();
    template.acceptance_criteria = vec!["status lands where the update says".into()];
    template.complexity = orbit_types::task::TaskComplexity::Medium;
    TestWorkspace::new("trimmed-surface-test", template)
}

impl TestWorkspace {
    fn drive_to_done(&self, id: &str) {
        self.run(&["task", "update", id, "--approve"], "approve");
        self.run(
            &[
                "task",
                "update",
                id,
                "--plan",
                "1) do it",
                "--status",
                "in-progress",
            ],
            "start",
        );
        self.run(
            &[
                "task",
                "update",
                id,
                "--execution-summary",
                "did it",
                "--status",
                "review",
            ],
            "to review",
        );
        self.run(&["task", "update", id, "--status", "done"], "to done");
    }

    /// Selectors the task's latest creation grant names, `null` for none.
    fn creation_grant(&self, id: &str) -> Value {
        let task = self.task_json(&["task", "show", id, "--json"]);
        task["history"]
            .as_array()
            .expect("task history")
            .iter()
            .rev()
            .find(|entry| entry["event"] == json!("context_creation_authorized"))
            .map(|entry| {
                let note: Value = serde_json::from_str(entry["note"].as_str().expect("grant note"))
                    .expect("grant JSON");
                note["selectors"].clone()
            })
            .unwrap_or(Value::Null)
    }

    fn task_json(&self, args: &[&str]) -> Value {
        let output = self.run(args, "task JSON command");
        serde_json::from_slice(&output.stdout).expect("task JSON output")
    }
}

fn run_orbit_with_identity(
    cwd: &Path,
    home: &Path,
    args: &[&str],
    agent: &str,
    model: &str,
) -> Output {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        // A managed identity this fixture synthesizes itself, on top of a
        // cleared environment — never the host run's (ORB-11300).
        .env("ORBIT_AGENT_NAME", agent)
        .env("ORBIT_AGENT_MODEL", model)
        .env("ORBIT_MANAGED_RUN_CONTEXT", "1")
        .env("ORBIT_RUN_ID", "task-trimmed-surface-managed")
        .args(args);
    command.output().expect("run orbit with managed identity")
}

#[test]
fn governed_status_transitions() {
    let workspace = workspace();
    for (name, check) in [
        (
            "update_status_reopens_terminal_tasks_only_with_an_explicit_force",
            update_status_reopens_terminal_tasks_only_with_an_explicit_force as fn(&TestWorkspace),
        ),
        (
            "update_status_restores_an_archived_task_with_force",
            update_status_restores_an_archived_task_with_force as fn(&TestWorkspace),
        ),
        (
            "update_status_refuses_completion_without_review_and_evidence",
            update_status_refuses_completion_without_review_and_evidence as fn(&TestWorkspace),
        ),
        (
            "update_status_performs_approve_and_reject_transitions",
            update_status_performs_approve_and_reject_transitions as fn(&TestWorkspace),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check(&workspace);
    }
}

#[test]
fn candidate_and_context_updates() {
    let workspace = workspace();
    for (name, check) in [
        (
            "update_discard_candidate_records_the_discard_unless_a_run_is_active",
            update_discard_candidate_records_the_discard_unless_a_run_is_active
                as fn(&TestWorkspace),
        ),
        (
            "task_update_repeats_dependencies_and_clears_context",
            task_update_repeats_dependencies_and_clears_context as fn(&TestWorkspace),
        ),
        (
            "task_update_rejects_missing_context_file_and_leaves_task_unchanged",
            task_update_rejects_missing_context_file_and_leaves_task_unchanged
                as fn(&TestWorkspace),
        ),
        (
            "task_update_and_add_accept_valid_context_selectors",
            task_update_and_add_accept_valid_context_selectors as fn(&TestWorkspace),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check(&workspace);
    }
}

#[test]
fn scope_readiness_and_creation_grants() {
    let workspace = workspace();
    for (name, check) in [
        (
            "tool_task_update_context_preserves_omissions_replaces_lists_and_clears",
            tool_task_update_context_preserves_omissions_replaces_lists_and_clears
                as fn(&TestWorkspace),
        ),
        (
            "task_show_and_write_tools_report_readiness_gaps",
            task_show_and_write_tools_report_readiness_gaps as fn(&TestWorkspace),
        ),
        (
            "backlog_writes_without_context_warn_about_the_missing_file_lock",
            backlog_writes_without_context_warn_about_the_missing_file_lock as fn(&TestWorkspace),
        ),
        (
            "allow_missing_context_records_durable_exact_creation_intent",
            allow_missing_context_records_durable_exact_creation_intent as fn(&TestWorkspace),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check(&workspace);
    }
}

#[test]
fn lock_and_audit_administration() {
    let workspace = workspace();
    for (name, check) in [
        (
            "locks_list_projects_files_held_by_active_tasks",
            locks_list_projects_files_held_by_active_tasks as fn(&TestWorkspace),
        ),
        (
            "locks_release_reaches_the_admin_tool_only_with_the_operator_capability",
            locks_release_reaches_the_admin_tool_only_with_the_operator_capability
                as fn(&TestWorkspace),
        ),
        (
            "audit_prune_refuses_unconfirmed_then_deletes_when_confirmed",
            audit_prune_refuses_unconfirmed_then_deletes_when_confirmed as fn(&TestWorkspace),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check(&workspace);
    }
}
