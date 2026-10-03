#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;

#[path = "support/isolated_cli_fixture.rs"]
mod isolated_cli_fixture;
use isolated_cli_fixture::Fixture;

#[test]
fn auto_task_cli_delete_opt_out_restore_and_open_task_refusals_persist() {
    let fixture = Fixture::new();
    let shipped = fixture.json(&["auto-task", "show", "backlog-hygiene", "--json"]);
    let shipped_path = PathBuf::from(shipped["definition_source"]["path"].as_str().unwrap());
    let original = fs::read(&shipped_path).unwrap();
    let removed = fixture.json(&[
        "auto-task",
        "delete",
        "backlog-hygiene",
        "--reason",
        "Disposable fixture opt-out",
        "--json",
    ]);
    assert_eq!(removed["opted_out"], true);
    assert!(!shipped_path.exists());
    fixture
        .command(&["auto-task", "show", "backlog-hygiene", "--json"])
        .assert()
        .failure();
    fixture
        .command(&["workspace", "sync", "--json"])
        .assert()
        .success();
    assert!(
        !shipped_path.exists(),
        "sync must honor the recorded opt-out"
    );
    let restored = fixture.json(&["auto-task", "restore", "backlog-hygiene", "--json"]);
    assert_eq!(restored["enabled"], false);
    assert_eq!(fs::read(&shipped_path).unwrap(), original);
    assert_eq!(
        fixture.json(&["auto-task", "show", "backlog-hygiene", "--json"])["enabled"],
        false
    );

    fixture.json(&[
        "auto-task",
        "add",
        "--name",
        "fixture-open-task",
        "--every-minutes",
        "60",
        "--title",
        "Fixture minted task",
        "--json",
    ]);
    let minted = fixture.json(&["auto-task", "mint", "fixture-open-task", "--json"]);
    let id = minted["id"].as_str().unwrap();
    let task_before = fixture.json(&["task", "show", id, "--json"]);
    let definition_before = fixture.json(&["auto-task", "show", "fixture-open-task", "--json"]);
    fixture
        .command(&[
            "auto-task",
            "delete",
            "fixture-open-task",
            "--reason",
            "Refused while open",
            "--json",
        ])
        .assert()
        .failure();
    assert_eq!(
        fixture.json(&["auto-task", "show", "fixture-open-task", "--json"]),
        definition_before
    );
    assert_eq!(fixture.json(&["task", "show", id, "--json"]), task_before);
    let forced = fixture.json(&[
        "auto-task",
        "delete",
        "fixture-open-task",
        "--reason",
        "Explicit disposable force",
        "--force",
        "--json",
    ]);
    assert_eq!(forced["opted_out"], false);
    assert!(
        forced["open_tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == id)
    );
    assert_eq!(fixture.json(&["task", "show", id, "--json"]), task_before);
    fixture
        .command(&["auto-task", "restore", "fixture-open-task", "--json"])
        .assert()
        .failure();
    assert_eq!(fixture.json(&["task", "show", id, "--json"]), task_before);
}

#[test]
fn auto_task_cli_recovery_and_reset_preview_preserve_then_audit_consumer_changes() {
    use chrono::Utc;
    use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
    use orbit_core::ActorIdentity;
    use orbit_core::application::automation::{consumer_key, evaluate_auto_task};

    let fixture = Fixture::new();
    let git = |args: &[&str]| {
        let result = std::process::Command::new("git")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
            ])
            .args(args)
            .current_dir(&fixture.repo)
            .env("HOME", &fixture.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_CONFIG_GLOBAL")
            .env_remove("GIT_CONFIG_SYSTEM")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "git fixture: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    git(&["checkout", "-b", "fixture-delivery"]);
    fs::write(fixture.repo.join("fixture.txt"), "baseline\n").unwrap();
    git(&["add", "fixture.txt"]);
    git(&["commit", "-m", "Disposable baseline"]);
    let mut trigger = serde_json::json!({"branch":"fixture-delivery","threshold":1,"max_wait_minutes":60,"coverage":"integrated_qa_v1","max_items":20,"retries":0});
    let schedule = trigger.to_string();
    fixture.json(&[
        "auto-task",
        "add",
        "--name",
        "fixture-delivery-consumer",
        "--deliveries-landed",
        &schedule,
        "--title",
        "Never dispatch in this fixture",
        "--json",
    ]);
    fixture.json(&[
        "auto-task",
        "toggle",
        "fixture-delivery-consumer",
        "on",
        "--json",
    ]);
    let roots = RegisteredRuntimeFactory::resolve_roots_for_cwd(&fixture.repo, Some(&fixture.root))
        .unwrap();
    assert!(roots.global_root.starts_with(fixture._temp.path()));
    assert!(roots.shared_root.starts_with(fixture._temp.path()));
    let runtime = RegisteredRuntimeFactory::open_resolved_roots(roots)
        .unwrap()
        .with_actor(ActorIdentity::human("fixture"));
    let definition = runtime
        .auto_task_show("fixture-delivery-consumer")
        .unwrap()
        .unwrap();
    // One public evaluation establishes the existing branch baseline. No landing,
    // obligation, provider process, or action is introduced.
    evaluate_auto_task(&runtime, &definition, false, Utc::now()).unwrap();
    let consumer = consumer_key(&runtime, "auto-task", &definition.name).unwrap();
    let store = runtime.automation_store().unwrap();
    let original = store.automation_state(&consumer).unwrap().unwrap();
    assert_eq!(original.pending.len(), 0);
    assert!(original.active.is_none());
    trigger["max_wait_minutes"] = serde_json::json!(120);
    fixture.json(&[
        "auto-task",
        "update",
        &definition.name,
        "--deliveries-landed",
        &trigger.to_string(),
        "--json",
    ]);
    let preview = fixture.json(&["auto-task", "recover", &definition.name, "--json"]);
    assert!(
        !preview["identity"]["changes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(preview["applied"].as_array().unwrap().is_empty());
    assert_eq!(
        store.automation_state(&consumer).unwrap(),
        Some(original.clone())
    );
    fixture
        .command(&[
            "auto-task",
            "recover",
            &definition.name,
            "--adopt-settings",
            "--json",
        ])
        .assert()
        .failure();
    assert_eq!(
        store.automation_state(&consumer).unwrap(),
        Some(original.clone())
    );
    let recovered = fixture.json(&[
        "auto-task",
        "recover",
        &definition.name,
        "--adopt-settings",
        "--reason",
        "Disposable settings adoption",
        "--json",
    ]);
    assert_eq!(
        recovered["applied"],
        serde_json::json!(["adopted_settings"])
    );
    let after = store.automation_state(&consumer).unwrap().unwrap();
    assert_ne!(after.epoch, original.epoch);
    assert_eq!(after.generation, original.generation + 1);
    assert_eq!(after.baseline, original.baseline);
    assert_eq!(after.pending, original.pending);
    let reset_preview = fixture.json(&["auto-task", "reset", &definition.name, "--json"]);
    assert_eq!(reset_preview["applied"], false);
    assert_eq!(reset_preview["generation"], after.generation);
    assert_eq!(store.automation_state(&consumer).unwrap(), Some(after));
    let reset = fixture.json(&[
        "auto-task",
        "reset",
        &definition.name,
        "--reason",
        "Disposable reset",
        "--json",
    ]);
    assert_eq!(reset["applied"], true);
    assert!(store.automation_state(&consumer).unwrap().is_none());
    let records = store.automation_recoveries(&consumer, 10).unwrap();
    assert_eq!(records.len(), 2);
    assert!(
        records
            .iter()
            .any(|record| record.reason == "Disposable settings adoption")
    );
    assert!(
        records
            .iter()
            .any(|record| record.reason == "Disposable reset")
    );
    let tasks = fixture.json(&["task", "list", "--json"]);
    assert_eq!(
        tasks.as_array().unwrap().len(),
        1,
        "consumer operations must not mint or dispatch a task"
    );
    fixture
        .command(&[
            "auto-task",
            "reset",
            "missing-consumer",
            "--reason",
            "Refused",
            "--json",
        ])
        .assert()
        .failure();
    assert_eq!(store.automation_recoveries(&consumer, 10).unwrap().len(), 2);
}
