//! CLI output must retain audit warnings after a committed tool mutation.

use std::path::Path;

use assert_cmd::cargo::cargo_bin_cmd;
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
fn audit_warning_survives_cli_field_and_artifact_put_projections() {
    let root = tempfile::tempdir_in(test_env::bulk_write_temp_dir()).expect("fixture");
    let home = root.path().join("home");
    let work = root.path().join("work");
    std::fs::create_dir_all(&home).expect("fixture home");
    git_repo::init(&work);

    fixture_orbit(&work, &home)
        .args(["workspace", "init", "--name", "audit-warning-fixture"])
        .assert()
        .success();

    let seed_input = json!({
        "title": "Audit warning artifact target",
        "description": "Target for the artifact.put output projection.",
        "acceptance_criteria": ["Keep the artifact write visible."],
        "complexity": "low",
        "model": "codex",
    })
    .to_string();
    let seed_output = fixture_orbit(&work, &home)
        .args([
            "tool",
            "run",
            "orbit.task.add",
            "--input",
            &seed_input,
            "--format",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let seed: Value = serde_json::from_slice(&seed_output).expect("seed task JSON");
    let task_id = seed["id"].as_str().expect("seed task ID").to_string();

    let database = orbit_config::resolved_audit_db_path(&orbit_config::ConfigRoots::new(
        home.join(".orbit"),
        work.join(".orbit"),
    ))
    .expect("audit database path");
    rusqlite::Connection::open(database)
        .expect("open audit database")
        .execute_batch(
            "CREATE TRIGGER injected_tool_audit_failure
             BEFORE INSERT ON audit_events
             WHEN NEW.command = 'tool'
               AND NEW.tool_name IN ('orbit.task.add', 'orbit.task.artifact.put')
             BEGIN
                 SELECT RAISE(ABORT, 'injected audit write failure');
             END;",
        )
        .expect("install audit failure trigger");

    let add_input = json!({
        "title": "Audit warning under fields",
        "description": "The task commits while its audit insert fails.",
        "acceptance_criteria": ["The CLI returns the audit warning."],
        "complexity": "low",
        "model": "codex",
    })
    .to_string();
    let add_output = fixture_orbit(&work, &home)
        .args([
            "tool",
            "run",
            "orbit.task.add",
            "--input",
            &add_input,
            "--fields",
            "id",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let added: Value = serde_json::from_slice(&add_output).expect("projected task JSON");
    assert!(added["id"].as_str().is_some(), "{added}");
    assert_eq!(
        added["audit_warning"]["code"], "audit_row_unpersisted",
        "{added}"
    );
    assert_eq!(
        added["audit_warning"]["committed"]["tool"], "orbit.task.add",
        "{added}"
    );
    assert!(
        added["audit_warning"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("injected audit write failure")),
        "the output must explain the missing audit row without process logs: {added}"
    );
    assert!(
        added.get("title").is_none(),
        "--fields id still filters fields: {added}"
    );

    let source_path = work.join(".orbit/tmp/audit-warning-source.txt");
    std::fs::create_dir_all(source_path.parent().expect("artifact parent"))
        .expect("artifact source directory");
    std::fs::write(&source_path, "committed artifact").expect("artifact source");
    let put_input = json!({
        "id": task_id,
        "path": "audit-warning.txt",
        "source_path": source_path.to_string_lossy(),
        "model": "codex",
    })
    .to_string();
    let put_output = fixture_orbit(&work, &home)
        .args([
            "tool",
            "run",
            "orbit.task.artifact.put",
            "--input",
            &put_input,
            "--format",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let put: Value = serde_json::from_slice(&put_output).expect("artifact put JSON");
    assert_eq!(put["id"], task_id, "{put}");
    assert_eq!(
        put["audit_warning"]["code"], "audit_row_unpersisted",
        "{put}"
    );
    assert_eq!(
        put["audit_warning"]["committed"]["tool"], "orbit.task.artifact.put",
        "{put}"
    );
    assert!(
        put["audit_warning"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("injected audit write failure")),
        "the default projection must explain the missing audit row: {put}"
    );

    let get_input = json!({"id": task_id, "path": "audit-warning.txt"}).to_string();
    let stored_output = fixture_orbit(&work, &home)
        .args([
            "tool",
            "run",
            "orbit.task.artifact.get",
            "--input",
            &get_input,
            "--format",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stored: Value = serde_json::from_slice(&stored_output).expect("stored artifact JSON");
    assert_eq!(stored["content"], "committed artifact", "{stored}");
}
