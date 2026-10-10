#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::fs::generation::{Access, GenerationGuard, Participant, ParticipantRole};
use orbit_common::test_env;
use serde_json::{Value, json};

use crate::git_repo;

fn fixture_orbit(work: &Path, home: &Path) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home);
    command
}

#[test]
fn dry_run_reports_activity_admission_without_executing_permitted_mutations() {
    let root = tempfile::tempdir().expect("fixture");
    let home = root.path().join("home");
    let work = root.path().join("work");
    std::fs::create_dir_all(&home).expect("fixture home");
    git_repo::init(&work);
    fixture_orbit(&work, &home)
        .args(["workspace", "init", "--name", "dry-run-fixture"])
        .assert()
        .success();
    let seed = fixture_orbit(&work, &home)
        .args([
            "tool", "run", "orbit.task.add", "--input",
            r#"{"title":"Existing fixture task","description":"Dry-run policy comparison","complexity":"low","model":"codex"}"#,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let seed: Value = serde_json::from_slice(&seed).expect("seeded task");
    let existing_task = json!({"id": seed["id"]}).to_string();

    for policy in ["allowlist", "deny"] {
        let managed = || {
            let mut command = fixture_orbit(&work, &home);
            command
                .env("ORBIT_TASK_ACTOR_KIND", "agent")
                .env("ORBIT_ACTIVITY_TOOLS", "orbit.task.add,orbit.task.list")
                .env("ORBIT_ACTIVITY_TOOL_POLICY", policy)
                .env("ORBIT_ACTIVITY_TOOLS_DENY", "orbit.task.show")
                .env("ORBIT_ACTIVITY_NAME", "dry-run-fixture");
            command
        };
        let output = managed()
            .args([
                "tool", "run", "orbit.task.add", "--dry-run", "--format", "json",
                "--input", r#"{"title":"Must not be created","description":"Preview only","complexity":"low","model":"codex"}"#,
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let allowed: Value = serde_json::from_slice(&output).expect("allowed preview");
        assert_eq!(allowed["policy_allowed"], true, "{allowed}");
        assert!(allowed["policy_denial_reason"].is_null(), "{allowed}");
        assert_eq!(allowed["missing_params"], json!([]));

        let output = managed()
            .args([
                "tool",
                "run",
                "orbit.task.show",
                "--dry-run",
                "--format",
                "json",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let denied: Value = serde_json::from_slice(&output).expect("denied preview");
        assert_eq!(denied["policy_allowed"], false, "{denied}");
        assert_eq!(denied["missing_params"], json!(["id"]));
        let reason = denied["policy_denial_reason"]
            .as_str()
            .expect("denial reason");
        assert!(!reason.is_empty());
        let real = managed()
            .args(["tool", "run", "orbit.task.show", "--input", &existing_task])
            .assert()
            .failure()
            .get_output()
            .clone();
        assert!(
            String::from_utf8_lossy(&real.stderr).contains(reason)
                || String::from_utf8_lossy(&real.stdout).contains(reason),
            "real dispatch and preview must report the same policy: {real:?}"
        );
        let human = managed()
            .args(["tool", "run", "orbit.task.show", "--dry-run"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        assert!(String::from_utf8_lossy(&human).contains(reason));

        let output = managed()
            .args(["tool", "run", "orbit.task.list", "--format", "json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let tasks: Value = serde_json::from_slice(&output).expect("task list");
        assert_eq!(tasks["total"], 1, "dry-run must not execute task.add");
    }
}

#[test]
fn dry_run_uses_capability_floors_including_input_dependent_authority() {
    let root = tempfile::tempdir().expect("fixture");
    let home = root.path().join("home");
    let work = root.path().join("work");
    std::fs::create_dir_all(&home).expect("fixture home");
    git_repo::init(&work);
    fixture_orbit(&work, &home)
        .args(["workspace", "init", "--name", "dry-run-capability"])
        .assert()
        .success();

    for (tool, input) in [
        ("orbit.routine.control", json!({"action": "status"})),
        (
            "orbit.auto_task.update",
            json!({"name": "missing", "expected_enabled": true, "enabled": false}),
        ),
    ] {
        for operator in [false, true] {
            let mut command = fixture_orbit(&work, &home);
            if operator {
                command.env("ORBIT_OPERATOR", "1");
            } else {
                command.env("ORBIT_TASK_ACTOR_KIND", "agent");
            }
            let output = command
                .args([
                    "tool",
                    "run",
                    tool,
                    "--input",
                    &input.to_string(),
                    "--dry-run",
                    "--format",
                    "json",
                ])
                .assert()
                .success()
                .get_output()
                .stdout
                .clone();
            let preview: Value = serde_json::from_slice(&output).expect("capability preview");
            assert_eq!(preview["policy_allowed"], operator, "{tool}: {preview}");
            assert_eq!(
                preview["policy_denial_reason"].is_null(),
                operator,
                "{preview}"
            );
            if !operator {
                assert!(
                    preview["policy_denial_reason"]
                        .as_str()
                        .is_some_and(|reason| reason.contains("orbit.routine.control")),
                    "{preview}"
                );
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn dry_run_reports_the_same_plugin_callback_allowlist_refusal_as_dispatch() {
    use std::os::fd::AsRawFd;

    let root = tempfile::tempdir().expect("fixture");
    let home = root.path().join("home");
    let work = root.path().join("work");
    std::fs::create_dir_all(&home).expect("fixture home");
    git_repo::init(&work);
    fixture_orbit(&work, &home)
        .args(["workspace", "init", "--name", "dry-run-callback"])
        .assert()
        .success();
    let sessions = home.join(".orbit/state/plugin-callbacks");
    std::fs::create_dir_all(&sessions).expect("callback sessions");
    let token = "b".repeat(64);
    let record = sessions.join(&token);
    let pid = std::process::id();
    let starttime = orbit_common::process::ancestry::process_start_key(pid)
        .expect("process start key")
        .starttime;
    std::fs::write(
        &record,
        json!({
            "schema_version": 3, "plugin": "ungranted", "version": "0.1.0",
            "manifest_digest": "0".repeat(64), "effective_tools": [],
            "token": token, "pid": pid, "starttime": starttime,
        })
        .to_string(),
    )
    .expect("callback record");
    let credential = std::fs::File::open(record).expect("open credential");
    // SAFETY: the file owns this descriptor and keeps it open through both children.
    assert!(unsafe { libc::fcntl(credential.as_raw_fd(), libc::F_SETFD, 0) } >= 0);
    let child = || {
        let mut command = fixture_orbit(&work, &home);
        command.env(
            "ORBIT_PLUGIN_CALLBACK_FD",
            credential.as_raw_fd().to_string(),
        );
        command
    };
    let output = child()
        .args([
            "tool",
            "run",
            "orbit.task.list",
            "--dry-run",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let preview: Value = serde_json::from_slice(&output).expect("callback preview");
    assert_eq!(preview["policy_allowed"], false, "{preview}");
    let reason = preview["policy_denial_reason"]
        .as_str()
        .expect("callback denial");
    assert!(reason.contains("orbit_tools allowlist"), "{reason}");
    let real = child()
        .args(["tool", "run", "orbit.task.list"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(
        String::from_utf8_lossy(&real.stderr).contains(reason)
            || String::from_utf8_lossy(&real.stdout).contains(reason),
        "callback admission must match real dispatch: {real:?}"
    );
}

#[test]
fn read_only_tool_run_persists_audit_while_joining_a_foreign_generation() {
    let root = tempfile::tempdir().expect("fixture");
    let home = root.path().join("home");
    let work = root.path().join("work");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&home).expect("fixture home");
    // Both cwds are independent checkouts, so neither resolves an enclosing
    // checkout's Orbit root when TMPDIR sits inside one.
    git_repo::init(&work);
    git_repo::init(&outside);
    fixture_orbit(&work, &home)
        .args(["workspace", "init", "--name", "audit-fixture"])
        .assert()
        .success();
    let added = fixture_orbit(&work, &home)
        .args([
            "tool",
            "run",
            "orbit.task.add",
            "--input",
            &json!({
                "title": "Read audit fixture", "description": "Disposable CLI audit regression",
                "complexity": "low", "model": "codex"
            })
            .to_string(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let task: Value = serde_json::from_slice(&added).expect("added task");
    let input = json!({"id": task["id"], "fields": ["status"]}).to_string();
    let global_root = home.join(".orbit");
    // A distinct live executable forces the actual CLI bootstrap to retain
    // read-only runtime handles, reproducing the lost-audit path.
    let identity = orbit_core::composition::compiled_compatibility();
    let digest = "a".repeat(64);
    let _record =
        GenerationGuard::acquire(&global_root, &digest).expect("record distinct executable digest");
    let _generation = GenerationGuard::join(
        &global_root,
        &Participant {
            digest: &digest,
            identity: &identity,
            role: ParticipantRole::Dashboard,
            access: Access::Write,
            handover: None,
            in_activity: false,
        },
        std::time::Duration::ZERO,
        || Ok(identity.store_schema.version),
    )
    .expect("live compatible foreign generation");
    let before = rusqlite::Connection::open_with_flags(
        global_root.join("orbit.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("fixture audit reader");
    let count = || {
        before
            .query_row(
                "SELECT count(*) FROM audit_events
                 WHERE tool_name = ?1 AND command = 'tool' AND subcommand = 'run'",
                ["orbit.task.show"],
                |row| row.get::<_, i64>(0),
            )
            .expect("audit count")
    };
    let initial = count();
    for cwd in [&work, &outside] {
        let output = fixture_orbit(cwd, &home)
            .args(["tool", "run", "orbit.task.show", "--input", &input])
            .assert()
            .success()
            .get_output()
            .clone();
        let value: Value = serde_json::from_slice(&output.stdout).expect("task status");
        assert_eq!(value, task["status"]);
        assert!(
            output.stderr.is_empty(),
            "read-only tool emitted a warning: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            std::fs::read_to_string(global_root.join(".generation.lock"))
                .expect("generation record")
                .contains(&digest),
            "the CLI must join the foreign generation without taking it over"
        );
    }
    assert_eq!(
        count(),
        initial + 2,
        "each CLI dispatch must persist exactly one audit row"
    );
}
