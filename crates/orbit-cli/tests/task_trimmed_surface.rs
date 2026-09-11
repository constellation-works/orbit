#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! End-to-end coverage for the task surface after ORB-10428:
//! direct audited status classification through `update --status` and
//! `prune-context` folded into `lint --fix`. Lock administration lives under
//! `task locks` (`list`/`release`).

use std::fs;
use std::path::Path;
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

#[test]
fn update_status_restores_archived_task_directly_to_any_status() {
    let workspace = TestWorkspace::new();
    let id = workspace.add_task("Restore me");
    workspace.run(&["task", "update", &id, "--status", "backlog"], "approve");
    workspace.run(&["task", "archive", &id], "archive");

    let restored =
        workspace.task_json(&["task", "update", &id, "--status", "in-progress", "--json"]);
    assert_eq!(restored["status"], json!("in-progress"));
}

#[test]
fn update_status_reopens_done_directly_and_accepts_accompanying_edits() {
    let workspace = TestWorkspace::new();
    let id = workspace.add_task("Terminal task");
    workspace.drive_to_done(&id);

    let reopened = workspace.task_json(&[
        "task",
        "update",
        &id,
        "--status",
        "rejected",
        "--title",
        "Reclassified task",
        "--json",
    ]);
    assert_eq!(reopened["status"], json!("rejected"));
    assert_eq!(reopened["title"], json!("Reclassified task"));
    assert!(!reopened["execution_summary"].as_str().unwrap().is_empty());

    let archived = workspace.task_json(&["task", "update", &id, "--status", "archived", "--json"]);
    assert_eq!(archived["status"], json!("archived"));
}

#[test]
fn update_status_performs_approve_and_reject_transitions() {
    let workspace = TestWorkspace::new();

    // proposed -> backlog (former `approve`).
    let id = workspace.add_task("Approve via update");
    let task = workspace.task_json(&["task", "update", &id, "--status", "backlog", "--json"]);
    assert_eq!(task["status"], json!("backlog"));

    // proposed -> rejected (former `reject`).
    let id = workspace.add_task("Reject via update");
    let task = workspace.task_json(&["task", "update", &id, "--status", "rejected", "--json"]);
    assert_eq!(task["status"], json!("rejected"));
}

#[test]
fn task_update_complexity_roundtrips_through_a_real_task_record() {
    let workspace = TestWorkspace::new();
    let id = workspace.add_task("Complexity update");

    let updated = workspace.task_json(&["task", "update", &id, "--complexity", "medium", "--json"]);
    assert_eq!(updated["complexity"], json!("medium"));

    let shown = workspace.task_json(&["task", "show", &id, "--json"]);
    assert_eq!(shown["complexity"], json!("medium"));
}

#[test]
fn task_update_repeats_dependencies_and_clears_context() {
    let workspace = TestWorkspace::new();
    let dependency_a = workspace.add_task("First dependency");
    let dependency_b = workspace.add_task("Second dependency");
    let id = workspace.add_task("List update");
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

#[test]
fn task_update_rejects_missing_context_file_and_leaves_task_unchanged() {
    let workspace = TestWorkspace::new();
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

#[test]
fn task_add_rejects_missing_context_file() {
    let workspace = TestWorkspace::new();
    let output = workspace.run_raw(&[
        "task",
        "add",
        "--title",
        "Add with missing context",
        "--complexity",
        "low",
        "--context",
        "file:does/not/exist.rs",
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("file:does/not/exist.rs"),
        "stderr must name selector: {stderr}"
    );
}

#[test]
fn task_update_and_add_accept_valid_context_selectors() {
    let workspace = TestWorkspace::new();
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

/// The existence guard is an operator-surface default, not a wall: work that
/// creates a file records its selector with the explicit escape.
#[test]
fn allow_missing_context_accepts_a_not_yet_existing_selector() {
    let workspace = TestWorkspace::new();

    let added = workspace.task_json(&[
        "task",
        "add",
        "--title",
        "Create a new module",
        "--complexity",
        "low",
        "--context",
        "file:src/future.rs",
        "--allow-missing-context",
        "--json",
    ]);
    assert_eq!(added["context_files"], json!(["file:src/future.rs"]));

    let id = added["id"].as_str().expect("task id");
    let updated = workspace.task_json(&[
        "task",
        "update",
        id,
        "--context",
        "file:src/other_future.rs",
        "--allow-missing-context",
        "--json",
    ]);
    assert_eq!(
        updated["context_files"],
        json!(["file:src/other_future.rs"])
    );
}

#[test]
fn locks_list_projects_files_held_by_active_tasks() {
    let workspace = TestWorkspace::new();
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
fn lint_fix_sweep_drops_stale_context_entries() {
    let workspace = TestWorkspace::new();
    fs::write(workspace.work.join("real.rs"), "// real\n").expect("write real file");
    fs::write(workspace.work.join("ghost.rs"), "// ghost\n").expect("write ghost file");

    let id = workspace.add_task("Has stale context");
    workspace.run(
        &[
            "task",
            "update",
            &id,
            "--context",
            "file:real.rs,file:ghost.rs",
        ],
        "set context files",
    );
    fs::remove_file(workspace.work.join("ghost.rs")).expect("remove ghost file");

    // Dry run first: reports the stale entry without writing.
    let dry = workspace.run(&["task", "lint", "--json"], "lint sweep dry run");
    let dry: Value = serde_json::from_slice(&dry.stdout).expect("dry-run JSON");
    assert_eq!(dry["dry_run"], json!(true));
    assert_eq!(dry["total_dropped"], json!(1));
    assert_eq!(dry["tasks_written"], json!(0));

    // Apply.
    let fixed = workspace.run(&["task", "lint", "--fix", "--json"], "lint sweep fix");
    let fixed: Value = serde_json::from_slice(&fixed.stdout).expect("fix JSON");
    assert_eq!(fixed["dry_run"], json!(false));
    assert_eq!(fixed["total_dropped"], json!(1));
    assert_eq!(fixed["tasks_written"], json!(1));
    assert_eq!(fixed["tasks"][0]["dropped"], json!(["file:ghost.rs"]));

    let task = workspace.task_json(&["task", "show", &id, "--json"]);
    assert_eq!(task["context_files"], json!(["file:real.rs"]));

    // Idempotent: nothing left to prune.
    let again = workspace.run(&["task", "lint", "--fix", "--json"], "lint sweep again");
    let again: Value = serde_json::from_slice(&again.stdout).expect("second fix JSON");
    assert_eq!(again["total_dropped"], json!(0));
}

#[test]
fn task_add_attributes_from_model_flag_and_managed_identity_env() {
    let ambient = TestWorkspace::new();
    let workspace = TestWorkspace::new();

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
        "gpt-5.6-sol",
        "--json",
    ]);
    assert_eq!(explicit["created_by"], json!("gpt-5.6-sol"));

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
        assert_eq!(managed["created_by"], json!("gpt-5.6-terra"));
    }

    let after = ambient.task_json(&["task", "list", "--json"]);
    assert_eq!(after, before, "ambient sentinel workspace was modified");

    let fixture_tasks = workspace.task_json(&["task", "list", "--json"]);
    let fixture_tasks = fixture_tasks
        .as_array()
        .cloned()
        .or_else(|| {
            fixture_tasks
                .get("tasks")
                .and_then(Value::as_array)
                .cloned()
        })
        .expect("fixture task list");
    assert_eq!(fixture_tasks.len(), 3);
    assert_eq!(
        fixture_tasks
            .iter()
            .filter(|task| task["created_by"] == json!("gpt-5.6-terra"))
            .count(),
        2
    );
}

#[test]
fn locks_release_reaches_the_admin_tool_only_with_the_operator_capability() {
    let workspace = TestWorkspace::new();
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

#[test]
fn audit_prune_refuses_unconfirmed_then_deletes_when_confirmed() {
    let workspace = TestWorkspace::new();
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

#[test]
fn run_cancel_confirmation_precedes_run_lookup() {
    let workspace = TestWorkspace::new();

    let refused = workspace.run_raw(&["run", "cancel", "jrun-does-not-exist"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--confirm"));

    let confirmed = workspace.run_raw(&["run", "cancel", "jrun-does-not-exist", "--confirm"]);
    assert!(!confirmed.status.success());
    let stderr = String::from_utf8_lossy(&confirmed.stderr);
    assert!(stderr.contains("jrun-does-not-exist"), "{stderr}");
    assert!(!stderr.contains("pass --confirm"), "{stderr}");
}

#[test]
fn migrate_bare_invocation_inspects_and_confirm_applies() {
    let workspace = TestWorkspace::new();
    let marker = workspace.work.join(".orbit/state/layout.version");
    fs::write(&marker, "1\n").expect("restore prior layout version");

    let preview = workspace.run_raw(&["migrate"]);
    assert!(!preview.status.success());
    assert_eq!(
        fs::read_to_string(&marker).expect("read previewed marker"),
        "1\n",
        "bare migrate must not advance the layout"
    );
    assert!(
        String::from_utf8_lossy(&preview.stderr).contains("migrate --confirm"),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );

    workspace.run(&["migrate", "--confirm"], "confirmed migration");
    assert_eq!(
        fs::read_to_string(&marker).expect("read applied marker"),
        "3\n",
        "confirmed migrate must advance the layout"
    );
}

#[test]
fn workspace_remove_is_recoverable_by_reinitializing_the_checkout() {
    let workspace = TestWorkspace::new();

    workspace.run_as_operator(
        &["workspace", "remove", "trimmed-surface-test"],
        "deregister workspace",
    );
    let unregistered = workspace.run(&["workspace", "show"], "show unregistered workspace");
    assert!(
        String::from_utf8_lossy(&unregistered.stdout).contains("not registered as a workspace")
    );

    workspace.run(
        &["workspace", "init", "--name", "trimmed-surface-test"],
        "reregister workspace",
    );
    let restored = workspace.run(&["workspace", "show"], "show restored workspace");
    assert!(String::from_utf8_lossy(&restored.stdout).contains("name:"));
    assert!(String::from_utf8_lossy(&restored.stdout).contains("trimmed-surface-test"));
}

struct TestWorkspace {
    _temp: TempDir,
    home: std::path::PathBuf,
    work: std::path::PathBuf,
}

impl TestWorkspace {
    fn new() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let work = home.join("work");
        fs::create_dir_all(&home).expect("create home");
        fs::create_dir_all(work.join(".git")).expect("create work repo");

        let workspace = Self {
            _temp: temp,
            home,
            work,
        };
        workspace.run(
            &["workspace", "init", "--name", "trimmed-surface-test"],
            "initialize workspace",
        );
        workspace
    }

    fn add_task(&self, title: &str) -> String {
        let output = self.run(
            &[
                "task",
                "add",
                "--title",
                title,
                "--description",
                "Created by the trimmed-surface integration test.",
                "--acceptance-criteria",
                "status lands where the update says",
                "--complexity",
                "medium",
                "--json",
            ],
            "add task",
        );
        let task: Value = serde_json::from_slice(&output.stdout).expect("task add JSON");
        assert_eq!(task["status"], json!("proposed"));
        task["id"].as_str().expect("task id").to_string()
    }

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

    fn task_json(&self, args: &[&str]) -> Value {
        let output = self.run(args, "task JSON command");
        serde_json::from_slice(&output.stdout).expect("task JSON output")
    }

    fn run(&self, args: &[&str], label: &str) -> Output {
        let output = self.run_raw(args);
        assert!(
            output.status.success(),
            "{label} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn run_raw(&self, args: &[&str]) -> Output {
        run_orbit(&self.work, &self.home, args)
    }

    /// Run a governed command as an explicit operator [ORB-10453].
    ///
    /// A test binary is not a terminal, so the capability chokepoint resolves
    /// it as an unidentified caller; claiming the capability is the same
    /// deliberate act the denial message asks for.
    fn run_as_operator(&self, args: &[&str], label: &str) -> Output {
        let output = run_orbit_as_operator(&self.work, &self.home, args);
        assert!(
            output.status.success(),
            "{label} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}

fn run_orbit(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(args);
    command.output().expect("run orbit")
}

fn run_orbit_as_operator(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("ORBIT_OPERATOR", "1")
        .args(args);
    command.output().expect("run orbit as operator")
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
