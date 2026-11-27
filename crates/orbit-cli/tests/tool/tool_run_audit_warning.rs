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
    if crate::isolated_test::run_in_child(
        "tool_run_audit_warning::audit_warning_survives_cli_field_and_artifact_put_projections",
    ) {
        return;
    }
    let root = tempfile::tempdir_in(test_env::bulk_write_temp_dir()).expect("fixture");
    let home = root.path().join("home");
    let work = root.path().join("work");
    std::fs::create_dir_all(&home).expect("fixture home");
    git_repo::init(&work);

    fixture_orbit(&work, &home)
        .args(["workspace", "init", "--name", "audit-warning-fixture"])
        .assert()
        .success();

    crate::fixture_crew::configure_sol(&home.join(".orbit"));
    let routine_path = work.join(".orbit/routines/resident_cycle.yaml");
    std::fs::create_dir_all(routine_path.parent().unwrap()).unwrap();
    std::fs::write(routine_path,
        "schemaVersion: 1\nname: artifact-cycle\nenabled: false\ntrigger:\n  cron: '0 * * * *'\ntarget: job:resident_cycle_pipeline\n",
    ).unwrap();

    let seed_input = json!({
        "title": "Audit warning artifact target",
        "tags": ["resident"],
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

    let database = orbit_config::resolved_audit_db_path(
        &orbit_common::Env::capture(),
        &orbit_config::ConfigRoots::new(home.join(".orbit"), work.join(".orbit")),
    )
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
    let put_input = json!({"action": "put",
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
            "orbit.task.artifact",
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

    let mandate = json!({
        "schema": "resident.mandate/v1",
        "goal": "Keep attachment warnings visible",
        "exit_criteria": ["The warning reaches the caller"],
        "workspaces": ["audit-warning-fixture"],
        "crew": "sol",
        "cadence": {"floor_minutes": 10, "heartbeat_minutes": 120}
    });
    std::fs::write(&source_path, mandate.to_string()).expect("mandate source");
    let mandate_input = json!({"action": "put",
        "id": task_id,
        "path": "resident/mandate.json",
        "source_path": source_path,
        "model": "codex",
    })
    .to_string();
    for flags in [vec!["--full"], vec![], vec!["--fields", "id"]] {
        let output = fixture_orbit(&work, &home)
            .args([
                "tool",
                "run",
                "orbit.task.artifact",
                "--input",
                &mandate_input,
                "--format",
                "json",
            ])
            .args(&flags)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let result: Value = serde_json::from_slice(&output).expect("mandate put JSON");
        assert_eq!(result["id"], task_id, "{result}");
        assert_eq!(
            result["audit_warning"]["code"], "audit_row_unpersisted",
            "{result}"
        );
        if flags.first() == Some(&"--fields") {
            assert_eq!(
                result.as_object().unwrap().len(),
                2,
                "explicit fields stay filtered: {result}"
            );
        } else {
            assert_eq!(result["status"], "in-progress", "{result}");
            assert_eq!(result["cycle_routine_disabled"], true, "{result}");
            let routine_name = result["cycle_routine_name"].as_str().expect("routine name");
            let disabled = format!("`{routine_name}` is disabled");
            assert!(
                result["warning"].as_str().is_some_and(|text| {
                    text.starts_with("Mandate stored, but ") && text.contains(&disabled)
                }),
                "{result}"
            );
            assert!(
                result["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|warning| {
                        warning
                            .as_str()
                            .is_some_and(|text| text.contains(&disabled))
                    }),
                "{result}"
            );
            assert_eq!(result["redactions_applied"], false, "{result}");
            assert_eq!(result["redactions"], json!([]), "{result}");
            assert_eq!(
                result.get("description").is_some(),
                flags.first() == Some(&"--full"),
                "{result}"
            );
        }
    }

    let native_output = fixture_orbit(&work, &home)
        .args([
            "task",
            "artifact",
            "put",
            &task_id,
            source_path.to_str().expect("UTF-8 fixture path"),
            "--path",
            "resident/mandate.json",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let native: Value = serde_json::from_slice(&native_output).expect("native artifact put JSON");
    assert_eq!(native["status"], "in-progress", "{native}");
    assert!(
        native["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| {
                warning.as_str().is_some_and(|text| {
                    text.contains("resident cycle routine") && text.contains("` is disabled")
                })
            }),
        "{native}"
    );

    let get_input =
        json!({"action": "get", "id": task_id, "path": "audit-warning.txt"}).to_string();
    let stored_output = fixture_orbit(&work, &home)
        .args([
            "tool",
            "run",
            "orbit.task.artifact",
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

#[test]
fn artifact_put_accepts_non_canonical_mandate_path_and_refuses_traversal() {
    if crate::isolated_test::run_in_child(
        "tool_run_audit_warning::artifact_put_accepts_non_canonical_mandate_path_and_refuses_traversal",
    ) {
        return;
    }
    let root = tempfile::tempdir_in(test_env::bulk_write_temp_dir()).expect("fixture");
    let home = root.path().join("home");
    let work = root.path().join("work");
    std::fs::create_dir_all(&home).expect("fixture home");
    git_repo::init(&work);
    fixture_orbit(&work, &home)
        .args(["workspace", "init", "--name", "artifact-put-fixture"])
        .assert()
        .success();
    let orbit_home = home.join(".orbit");
    crate::fixture_crew::configure_sol(&orbit_home);
    let config = orbit_home.join("config.toml");
    let mut settings = std::fs::read_to_string(&config).expect("fixture config");
    settings.push_str("\n[crews.mandate-fixture]\nprovider = \"codex\"\nmodel = \"gpt-6-sol\"\n");
    std::fs::write(config, settings).expect("mandate crew config");
    let routine_path = work.join(".orbit/routines/resident_cycle.yaml");
    std::fs::create_dir_all(routine_path.parent().unwrap()).unwrap();
    std::fs::write(
        routine_path,
        "schemaVersion: 1\nname: artifact-cycle\nenabled: false\ntrigger:\n  cron: '0 * * * *'\ntarget: job:resident_cycle_pipeline\n",
    )
    .unwrap();
    let source_path = work.join(".orbit/tmp/mandate.json");
    std::fs::create_dir_all(source_path.parent().expect("artifact source parent"))
        .expect("artifact source directory");
    let mandate = json!({
        "schema": "resident.mandate/v1",
        "goal": "Keep direct artifact writes equivalent",
        "exit_criteria": ["Canonical and prefixed paths behave the same"],
        "workspaces": ["artifact-put-fixture"],
        "crew": "mandate-fixture",
        "cadence": {"floor_minutes": 10, "heartbeat_minutes": 120}
    });
    std::fs::write(&source_path, mandate.to_string()).expect("mandate source");
    let source = source_path.to_str().expect("UTF-8 fixture path");

    let add_resident = || {
        let output = fixture_orbit(&work, &home)
            .args([
                "task",
                "add",
                "--title",
                "Artifact put resident",
                "--status",
                "backlog",
                "--complexity",
                "low",
                "--tag",
                "resident",
                "--crew",
                "sol",
                "--json",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let task: Value = serde_json::from_slice(&output).expect("resident task JSON");
        task["id"].as_str().expect("resident task ID").to_string()
    };
    let tool_put = |id: &str, path: &str| {
        let input = json!({
            "action": "put",
            "id": id,
            "path": path,
            "source_path": source,
            "model": "codex",
        })
        .to_string();
        let output = fixture_orbit(&work, &home)
            .args([
                "tool",
                "run",
                "orbit.task.artifact",
                "--input",
                &input,
                "--format",
                "json",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice::<Value>(&output).expect("artifact put JSON")
    };

    let control_id = add_resident();
    let control = tool_put(&control_id, "resident/mandate.json");
    let alternate_id = add_resident();
    let alternate = tool_put(&alternate_id, "./resident/mandate.json");
    let control_task: Value = serde_json::from_slice(
        &fixture_orbit(&work, &home)
            .args(["task", "show", &control_id, "--json"])
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .expect("control task JSON");
    let alternate_task: Value = serde_json::from_slice(
        &fixture_orbit(&work, &home)
            .args(["task", "show", &alternate_id, "--json"])
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .expect("alternate task JSON");
    let control_resident: Value = serde_json::from_slice(
        &fixture_orbit(&work, &home)
            .args(["resident", "show", &control_id, "--json"])
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .expect("control resident JSON");
    let alternate_resident: Value = serde_json::from_slice(
        &fixture_orbit(&work, &home)
            .args(["resident", "show", &alternate_id, "--json"])
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .expect("alternate resident JSON");
    let metadata = |result: &Value, task: &Value, resident: &Value| {
        json!({
            "crew": task["crew"],
            "task_status": task["status"],
            "resident_status": resident["status"],
            "cycle_routine_disabled": result["cycle_routine_disabled"],
            "cycle_routine_name": result["cycle_routine_name"],
            "warning": result["warning"],
        })
    };
    assert_eq!(
        metadata(&alternate, &alternate_task, &alternate_resident),
        metadata(&control, &control_task, &control_resident)
    );
    for (result, task, resident) in [
        (&control, &control_task, &control_resident),
        (&alternate, &alternate_task, &alternate_resident),
    ] {
        assert_eq!(task["crew"], "mandate-fixture", "{task}");
        assert_eq!(task["status"], "in-progress", "{task}");
        assert_eq!(resident["status"], "in_progress", "{resident}");
        assert_eq!(resident["cycle_routine_disabled"], true, "{resident}");
        assert_eq!(result["cycle_routine_disabled"], true, "{result}");
        let routine_name = resident["cycle_routine_name"].as_str().unwrap();
        assert_eq!(result["cycle_routine_name"], routine_name, "{result}");
        assert!(
            result["warning"]
                .as_str()
                .is_some_and(|warning| warning.contains(&format!("`{routine_name}` is disabled"))),
            "{result}"
        );
        assert!(
            task["artifacts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|artifact| { artifact["path"] == "resident/mandate.json" }),
            "{task}"
        );
    }

    let invalid_id = add_resident();
    let before_output = fixture_orbit(&work, &home)
        .args(["task", "show", &invalid_id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let before: Value = serde_json::from_slice(&before_output).expect("before task JSON");
    let before_state = json!({
        "crew": before["crew"],
        "status": before["status"],
        "artifacts": before["artifacts"],
    });
    let invalid_input = json!({
        "action": "put",
        "id": invalid_id,
        "path": "../resident/mandate.json",
        "source_path": source,
        "model": "codex",
    })
    .to_string();
    fixture_orbit(&work, &home)
        .args([
            "tool",
            "run",
            "orbit.task.artifact",
            "--input",
            &invalid_input,
            "--format",
            "json",
        ])
        .assert()
        .failure();
    let after_output = fixture_orbit(&work, &home)
        .args(["task", "show", &invalid_id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let after: Value = serde_json::from_slice(&after_output).expect("after task JSON");
    assert_eq!(
        json!({
            "crew": after["crew"],
            "status": after["status"],
            "artifacts": after["artifacts"],
        }),
        before_state,
        "invalid artifact paths must not write or activate the resident"
    );
    assert_eq!(after["crew"], "sol", "{after}");
    assert_eq!(after["status"], "backlog", "{after}");
}
