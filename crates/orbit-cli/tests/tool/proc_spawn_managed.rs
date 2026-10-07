#![allow(missing_docs)]
// Integration fixtures use expect for concise failure diagnostics.
#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::{Command as StdCommand, Output};

use assert_cmd::Command as AssertCommand;
use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::{Value, json};
use tempfile::tempdir;

/// A managed CLI worker already runs inside its OS sandbox, and its
/// `proc.spawn` children inherit that view: the activity `fsProfile` neither
/// refuses a path argument nor is required at all. What the managed context
/// still decides is the program policy it hands the nested `orbit`.
#[test]
fn managed_cli_proc_spawn_enforces_program_policy_and_inherits_parent_reads() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(workspace.join("allowed")).expect("create allowed directory");
    fs::create_dir_all(workspace.join("denied")).expect("create denied directory");
    fs::write(workspace.join("allowed/visible.txt"), "visible").expect("write allowed fixture");
    fs::write(workspace.join("denied/private.txt"), "private").expect("write denied fixture");
    init_git_repo(&workspace);
    workspace_init(&workspace, &home);
    fs::write(
        home.join(".orbit/resources/policies/default.yaml"),
        r#"schemaVersion: 2
kind: Policy
metadata:
  name: default
spec:
  description: Managed proc.spawn integration policy
  denyRead:
    - ./denied/**
  denyModify: []
  fsProfiles:
    restricted:
      read:
        - ./allowed/**
      modify: []
"#,
    )
    .expect("write restricted policy");

    for (path, profile, expected) in [
        ("allowed/visible.txt", Some("restricted"), "visible"),
        // No enclosing OS mask covers this path here, so the child reads
        // exactly what its parent can.
        ("denied/private.txt", Some("restricted"), "private"),
        ("allowed/visible.txt", None, "visible"),
    ] {
        let output = run_managed_proc_spawn(
            &workspace,
            &home,
            json!({ "program": "/bin/cat", "args": [path], "timeout_ms": 5_000 }),
            profile,
        );
        assert!(
            output.status.success(),
            "managed proc.spawn of {path} (profile {profile:?}) failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).expect("JSON output");
        assert_eq!(value["stdout"].as_str(), Some(expected), "{value}");
    }

    let refused = run_managed_proc_spawn(
        &workspace,
        &home,
        json!({ "program": "/bin/ls", "args": ["allowed"], "timeout_ms": 5_000 }),
        Some("restricted"),
    );
    assert!(
        !refused.status.success(),
        "a program outside ORBIT_PROC_ALLOWED_PROGRAMS ran\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&refused.stdout).contains("visible.txt"),
        "the refused program's output reached the caller"
    );
}

/// A managed run's nested `orbit tool run proc.spawn` may run as long as the
/// activity has left, under the operator's ceiling; the deadline is honored
/// only behind the managed-run envelope.
#[test]
fn managed_proc_spawn_timeout_ceiling_follows_the_activity_deadline() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&workspace).expect("create workspace");
    init_git_repo(&workspace);
    workspace_init(&workspace, &home);
    let output = orbit_command(&workspace, &home)
        .args([
            "config",
            "set",
            "--global",
            "execution.proc_spawn_max_timeout_minutes",
            "3",
        ])
        .output()
        .expect("run config set");
    assert!(
        output.status.success(),
        "config set failed\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let now_ms = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_millis(),
    )
    .expect("epoch millis fit u64");
    let input = json!({ "program": "/bin/cat", "args": ["/dev/null"], "timeout_ms": 600_000 });
    let run = |managed: bool, deadline_ms: Option<u64>| -> Value {
        let mut command = orbit_command(&workspace, &home);
        if managed {
            command
                .env("ORBIT_MANAGED_RUN_CONTEXT", "1")
                .env("ORBIT_RUN_ID", "jrun-proc-spawn-test")
                .env("ORBIT_TASK_ACTOR_KIND", "agent")
                .env("ORBIT_ACTIVITY_TOOLS", "proc.spawn")
                .env("ORBIT_PROC_ALLOWED_PROGRAMS", "/bin/cat");
        }
        if let Some(deadline_ms) = deadline_ms {
            command.env("ORBIT_ACTIVITY_DEADLINE_UNIX_MS", deadline_ms.to_string());
        }
        let output = command
            .args(["tool", "run", "proc.spawn", "--input", &input.to_string()])
            .output()
            .expect("run proc.spawn");
        assert!(
            output.status.success(),
            "proc.spawn failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("JSON output")
    };

    // An hour left: the configured three minutes bound the call.
    let value = run(true, Some(now_ms + 3_600_000));
    assert_eq!(value["timeout_ms"], json!(180_000), "{value}");
    assert_eq!(
        value["timeout_ceiling_source"],
        json!("configured"),
        "{value}"
    );
    assert_eq!(value["timeout_clamped"], json!(true), "{value}");

    // Two minutes left: the activity's remaining budget is the smaller bound.
    let value = run(true, Some(now_ms + 120_000));
    let applied = value["timeout_ms"].as_u64().expect("timeout_ms");
    assert!(applied <= 120_000 && applied > 60_000, "{value}");
    assert_eq!(
        value["timeout_ceiling_source"],
        json!("activity_remaining"),
        "{value}"
    );

    // No deadline in the envelope, or no managed envelope at all: the fixed
    // 60 s ceiling, whatever the environment claims.
    for (managed, deadline_ms) in [(true, None), (false, Some(now_ms + 3_600_000))] {
        let value = run(managed, deadline_ms);
        assert_eq!(value["timeout_ms"], json!(60_000), "{value}");
        assert_eq!(
            value["timeout_ceiling_source"],
            json!("unscoped"),
            "{value}"
        );
    }
}

/// A task-pilot provider runs in an invocation-owned checkout pinned to the
/// prepared commit while its Orbit identity stays on the registered
/// workspace. [ORB-13800] Its nested `orbit tool run proc.spawn` used to fall
/// back to the registered primary because that checkout is a standalone
/// repository: `git`/`rg` then reported the primary's later HEAD and content
/// while the provider's own reads saw the pinned snapshot.
#[test]
fn managed_pilot_proc_spawn_inspects_the_pinned_checkout_not_the_primary() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let primary = temp.path().join("primary");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&primary).expect("create primary");
    init_git_repo(&primary);
    fs::write(primary.join("subject.txt"), "pinned snapshot\n").expect("write pinned subject");
    run_git(&primary, &["add", "subject.txt"]);
    run_git(&primary, &["commit", "-m", "prepared"]);
    let pinned = git_stdout(&primary, &["rev-parse", "HEAD"]);
    workspace_init(&primary, &home);
    let workspace_id = registered_workspace_id(&primary, &home);
    let task_id = add_task(&primary, &home);

    // The primary moves on after preparation and carries an operator's
    // in-flight edits: a later commit, an unstaged edit, a staged file and an
    // untracked file. None of it may leak into, or be touched by, inspection.
    fs::write(primary.join("subject.txt"), "primary later\n").expect("write later subject");
    run_git(&primary, &["commit", "-am", "later"]);
    fs::write(primary.join("README.md"), "# unstaged edit\n").expect("write unstaged edit");
    fs::write(primary.join("staged.txt"), "staged\n").expect("write staged file");
    run_git(&primary, &["add", "staged.txt"]);
    fs::write(primary.join("untracked.txt"), "untracked\n").expect("write untracked file");
    let primary_head = git_stdout(&primary, &["rev-parse", "HEAD"]);
    assert_ne!(
        primary_head, pinned,
        "fixture primary must diverge from the pin"
    );
    let primary_before = primary_state(&primary);

    let checkout = materialize_inspection_slot(&primary, &pinned);
    let checkout_root = checkout.canonicalize().expect("canonical checkout");

    // The provider's native view: its cwd is the slot at the pinned revision.
    assert_eq!(
        fs::read_to_string(checkout.join("subject.txt")).expect("native read"),
        "pinned snapshot\n"
    );
    assert_eq!(git_stdout(&checkout, &["rev-parse", "HEAD"]), pinned);

    let mut inspections = vec![
        (
            json!({ "program": "git", "args": ["rev-parse", "--show-toplevel"] }),
            checkout_root.display().to_string(),
        ),
        (
            json!({ "program": "git", "args": ["rev-parse", "HEAD"] }),
            pinned.clone(),
        ),
        (
            json!({ "program": "git", "args": ["grep", "-h", "snapshot", "--", "subject.txt"] }),
            "pinned snapshot".to_string(),
        ),
    ];
    if on_path("rg") {
        inspections.push((
            json!({ "program": "rg", "args": ["--no-filename", "snapshot|later", "subject.txt"] }),
            "pinned snapshot".to_string(),
        ));
    }
    for (input, expected) in inspections {
        let output = run_pilot_tool(&checkout, &home, &workspace_id, "proc.spawn", &input);
        assert!(
            output.status.success(),
            "pilot proc.spawn {input} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).expect("JSON output");
        assert_eq!(value["exit_code"], json!(0), "{input}: {value}");
        assert_eq!(
            value["stdout"].as_str().map(str::trim),
            Some(expected.as_str()),
            "pilot proc.spawn {input} must inspect the pinned checkout, not the primary"
        );
    }

    // Task tools keep routing to the registered owning workspace.
    let shown = run_pilot_tool(
        &checkout,
        &home,
        &workspace_id,
        "orbit.task.show",
        &json!({ "id": task_id, "fields": ["id", "title"], "model": "claude" }),
    );
    assert!(
        shown.status.success(),
        "pilot orbit.task.show failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&shown.stdout),
        String::from_utf8_lossy(&shown.stderr)
    );
    let shown: Value = serde_json::from_slice(&shown.stdout).expect("task JSON");
    assert_eq!(shown["id"].as_str(), Some(task_id.as_str()), "{shown}");

    // Program policy still binds the pilot in the pinned checkout.
    let refused = run_pilot_tool(
        &checkout,
        &home,
        &workspace_id,
        "proc.spawn",
        &json!({ "program": "/bin/cat", "args": ["subject.txt"] }),
    );
    assert!(
        !refused.status.success(),
        "a program outside ORBIT_PROC_ALLOWED_PROGRAMS ran\nstdout:\n{}",
        String::from_utf8_lossy(&refused.stdout)
    );

    assert_eq!(git_stdout(&checkout, &["rev-parse", "HEAD"]), pinned);
    assert_eq!(
        git_stdout(
            &checkout,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        "",
        "inspection must leave the pinned checkout clean"
    );
    assert_eq!(
        primary_state(&primary),
        primary_before,
        "pilot inspection changed the primary checkout"
    );
}

/// Lay out an inspection slot the way the CLI runner does: a standalone
/// repository under the primary's Orbit state, fetched at the pinned revision
/// and detached, beside its owner marker and lease, with run scratch inside.
fn materialize_inspection_slot(primary: &Path, revision: &str) -> std::path::PathBuf {
    let slot = primary.join(".orbit/state/source-inspections-v1/0");
    let checkout = slot.join("checkout");
    fs::create_dir_all(&checkout).expect("create inspection checkout");
    fs::write(slot.join("owner"), "orbit-source-inspection-v1\n").expect("write owner marker");
    fs::write(slot.join("lease"), "").expect("write lease");
    run_git(&checkout, &["init", "--quiet", "--template="]);
    let common = primary.join(".git");
    run_git(
        &checkout,
        &[
            "-c",
            "protocol.file.allow=always",
            "fetch",
            "--quiet",
            "--no-tags",
            common.to_str().expect("utf8 git dir"),
            revision,
        ],
    );
    run_git(&checkout, &["checkout", "--quiet", "--detach", revision]);
    fs::create_dir_all(checkout.join(".orbit/tmp")).expect("create inspection scratch");
    checkout
}

/// The primary's HEAD, index bytes, staged and unstaged diffs, and every
/// untracked path with its content.
fn primary_state(primary: &Path) -> Vec<String> {
    let mut state = vec![
        git_stdout(primary, &["rev-parse", "HEAD"]),
        git_stdout(primary, &["ls-files", "--stage", "--debug"]),
        git_stdout(primary, &["diff", "--binary"]),
        git_stdout(primary, &["diff", "--cached", "--binary"]),
        format!(
            "{:?}",
            fs::read(primary.join(".git/index")).expect("read primary index")
        ),
    ];
    for path in git_stdout(primary, &["ls-files", "--others", "--exclude-standard"]).lines() {
        state.push(format!(
            "{path}: {}",
            fs::read_to_string(primary.join(path)).expect("read untracked file")
        ));
    }
    state
}

fn registered_workspace_id(workspace: &Path, home: &Path) -> String {
    let output = orbit_command(workspace, home)
        .args(["workspace", "list", "--format", "json"])
        .output()
        .expect("run workspace list");
    assert!(output.status.success(), "workspace list failed: {output:?}");
    let rows: Value = serde_json::from_slice(&output.stdout).expect("workspace list JSON");
    rows.as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row["id"].as_str())
        .expect("registered workspace id")
        .to_string()
}

fn add_task(workspace: &Path, home: &Path) -> String {
    let input = json!({
        "title": "Pinned inspection routing fixture",
        "description": "Task the pilot reads through its owning workspace.",
        "complexity": "low",
        "model": "claude"
    });
    let output = orbit_command(workspace, home)
        .args([
            "tool",
            "run",
            "orbit.task.add",
            "--input",
            &input.to_string(),
        ])
        .output()
        .expect("run orbit.task.add");
    assert!(
        output.status.success(),
        "orbit.task.add failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let task: Value = serde_json::from_slice(&output.stdout).expect("task JSON");
    task["id"].as_str().expect("task id").to_string()
}

/// A nested `orbit tool run` under a source-inspection provider's envelope:
/// managed provenance with an invocation session but no job run, the logical
/// workspace selector, and the pilot's tool and program grants.
fn run_pilot_tool(
    checkout: &Path,
    home: &Path,
    workspace_id: &str,
    tool: &str,
    input: &Value,
) -> Output {
    orbit_command(checkout, home)
        .env("ORBIT_MANAGED_RUN_CONTEXT", "1")
        .env("ORBIT_SESSION_ID", "pilot-inspection-session")
        .env("ORBIT_WORKSPACE", workspace_id)
        .env("ORBIT_TASK_ACTOR_KIND", "agent")
        .env("ORBIT_ACTIVITY_TOOLS", "proc.spawn,orbit.task.show")
        .env("ORBIT_ACTIVITY_FS_PROFILE", "reviewer")
        .env("ORBIT_PROC_ALLOWED_PROGRAMS", "git,rg")
        .env_remove("ORBIT_RUN_ID")
        .args(["tool", "run", tool, "--input", &input.to_string()])
        .output()
        .expect("run pilot tool")
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

fn git_stdout(workspace: &Path, args: &[&str]) -> String {
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git -C {} {} failed\nstderr:\n{}",
        workspace.display(),
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("utf8 git output")
        .trim()
        .to_string()
}

fn workspace_init(workspace: &Path, home: &Path) {
    let output = orbit_command(workspace, home)
        .args(["workspace", "init"])
        .output()
        .expect("run workspace init");
    assert!(
        output.status.success(),
        "workspace init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_managed_proc_spawn(
    workspace: &Path,
    home: &Path,
    input: Value,
    fs_profile: Option<&str>,
) -> Output {
    let mut command = orbit_command(workspace, home);
    command
        .env("ORBIT_MANAGED_RUN_CONTEXT", "1")
        .env("ORBIT_RUN_ID", "jrun-proc-spawn-test")
        .env("ORBIT_TASK_ACTOR_KIND", "agent")
        .env("ORBIT_ACTIVITY_TOOLS", "proc.spawn")
        .env("ORBIT_PROC_ALLOWED_PROGRAMS", "/bin/cat");
    match fs_profile {
        Some(profile) => {
            command.env("ORBIT_ACTIVITY_FS_PROFILE", profile);
        }
        None => {
            command.env_remove("ORBIT_ACTIVITY_FS_PROFILE");
        }
    }
    command
        .args(["tool", "run", "proc.spawn", "--input", &input.to_string()])
        .output()
        .expect("run managed proc.spawn")
}

fn orbit_command(workspace: &Path, home: &Path) -> AssertCommand {
    let mut command = cargo_bin_cmd!("orbit");
    // ORB-11300: `run_managed_proc_spawn` synthesizes its *own* managed run
    // context on top of this. Without clearing the inherited
    // `ORBIT_REGISTRY_ROOT`/`ORBIT_WORKSPACE` pair first, that synthetic
    // context would have pointed the sandboxed tool at the live workspace.
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(workspace)
        .env("HOME", home)
        .env("USERPROFILE", home);
    command
}

fn init_git_repo(workspace: &Path) {
    run_git(workspace, &["init"]);
    run_git(workspace, &["config", "user.name", "Orbit Test"]);
    run_git(
        workspace,
        &["config", "user.email", "orbit-test@example.com"],
    );
    run_git(workspace, &["config", "commit.gpgsign", "false"]);
    fs::write(workspace.join("README.md"), "# fixture\n").expect("write readme");
    run_git(workspace, &["add", "README.md"]);
    run_git(workspace, &["commit", "-m", "initial"]);
}

fn run_git(workspace: &Path, args: &[&str]) {
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git -C {} {} failed\nstdout:\n{}\nstderr:\n{}",
        workspace.display(),
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
