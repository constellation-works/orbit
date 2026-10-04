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
