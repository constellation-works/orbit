#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use crate::isolated_cli_fixture;
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
    use orbit_core::application::automation::consumer_key;

    let fixture = Fixture::new();
    let mut trigger = serde_json::json!({"branch":"fixture-delivery","threshold":1,"max_wait_minutes":60,"coverage":"integrated_qa_v1","max_items":20,"retries":0});
    let (runtime, definition) = baselined_delivery_consumer(&fixture, &trigger);
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

/// Run one git command in the fixture repository, isolated from the caller's
/// configuration, and return its trimmed stdout.
fn git(fixture: &Fixture, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&result.stdout).trim().to_string()
}

/// An enabled delivery auto-task over a disposable `fixture-delivery` branch,
/// baselined by one public evaluation. No landing, obligation, provider
/// process, or action is introduced.
fn baselined_delivery_consumer(
    fixture: &Fixture,
    trigger: &serde_json::Value,
) -> (
    orbit_core::OrbitRuntime,
    orbit_types::workflow::AutoTaskDefinition,
) {
    use chrono::Utc;
    use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
    use orbit_core::ActorIdentity;
    use orbit_core::application::automation::evaluate_auto_task;

    git(fixture, &["checkout", "-b", "fixture-delivery"]);
    fs::write(fixture.repo.join("fixture.txt"), "baseline\n").unwrap();
    git(fixture, &["add", "fixture.txt"]);
    git(fixture, &["commit", "-m", "Disposable baseline"]);
    fixture.json(&[
        "auto-task",
        "add",
        "--name",
        "fixture-delivery-consumer",
        "--deliveries-landed",
        &trigger.to_string(),
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
    evaluate_auto_task(&runtime, &definition, false, Utc::now()).unwrap();
    (runtime, definition)
}

/// A review task that closed without accepted coverage once left its consumer
/// `admitted` forever: reset and recover refused it as executing and nothing
/// reported it. Its task being terminal is what makes it not executing.
#[test]
fn auto_task_cli_reset_recover_and_doctor_treat_a_closed_action_as_settled() {
    use chrono::Utc;
    use orbit_core::application::automation::consumer_key;
    use orbit_types::workflow::automation::{
        BatchAttempt, BatchState, CoverageBatch, SourceRevision,
    };

    let fixture = Fixture::new();
    let trigger = serde_json::json!({"branch":"fixture-delivery","threshold":1,"max_wait_minutes":60,"coverage":"landed_code_review_v1","max_items":20,"retries":1});
    let (runtime, definition) = baselined_delivery_consumer(&fixture, &trigger);
    let consumer = consumer_key(&runtime, "auto-task", &definition.name).unwrap();
    let store = runtime.automation_store().unwrap();
    let baselined = store.automation_state(&consumer).unwrap().unwrap();

    // One landed commit, frozen into a batch and admitted to a review task.
    fs::write(fixture.repo.join("fixture.txt"), "landed\n").unwrap();
    git(&fixture, &["commit", "-am", "Disposable landing"]);
    let landed = SourceRevision {
        commit: git(&fixture, &["rev-parse", "HEAD"]),
        tree: git(&fixture, &["rev-parse", "HEAD^{tree}"]),
    };
    let review = fixture.json(&[
        "task",
        "add",
        "--title",
        "Review the frozen batch",
        "--complexity",
        "low",
        "--acceptance-criteria",
        "Attach coverage evidence",
        "--json",
    ]);
    let action_id = review["id"].as_str().unwrap().to_string();
    let now = Utc::now();
    let batch = CoverageBatch {
        schema_version: 1,
        id: "fixture-batch".into(),
        consumer: consumer.clone(),
        epoch: baselined.epoch.clone(),
        repository: baselined.repository.clone(),
        branch: baselined.branch.clone(),
        coverage: baselined.trigger.as_ref().unwrap().coverage,
        from_exclusive: baselined.covered.clone(),
        through_inclusive: landed.clone(),
        commits: vec![landed.commit.clone()],
        deliveries: vec![],
        exclusions: vec![],
        created_at: now,
        max_attempts: 2,
        retry_until: now + chrono::Duration::hours(24),
    };
    let mut admitted = baselined.clone();
    admitted.generation += 1;
    admitted.observed = landed;
    admitted.pending_commits = batch.commits.clone();
    admitted.active = Some(BatchAttempt {
        action_key: "automation:fixture-batch:1".into(),
        batch,
        input_digest: "fixture-input".into(),
        attempt: 1,
        action_id: Some(action_id.clone()),
        state: BatchState::Admitted,
        reason: None,
        retry_after: None,
        reissue: None,
    });
    assert!(
        store
            .automation_commit(&baselined, &admitted, None)
            .unwrap()
    );

    let doctor_row = || {
        let output = fixture.command(&["doctor", "--json"]).output().unwrap();
        let rows: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        rows.as_array()
            .unwrap()
            .iter()
            .find(|row| row["check"] == "automation-consumers")
            .cloned()
            .unwrap()
    };

    // While the task is open its executor may still submit evidence.
    let preview = fixture.json(&["auto-task", "reset", &definition.name, "--json"]);
    assert!(
        preview["refusals"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("action_executing"))
    );
    assert!(
        !doctor_row()["message"].as_str().unwrap().contains("wedged"),
        "an open action is not wedged"
    );

    fixture.json(&[
        "task", "update", &action_id, "--status", "rejected", "--force", "--json",
    ]);

    let row = doctor_row();
    assert_eq!(row["status"], "warning");
    let message = row["message"].as_str().unwrap();
    assert!(
        message.contains("wedged") && message.contains(&definition.name),
        "{message}"
    );

    let recover = fixture.json(&["auto-task", "recover", &definition.name, "--json"]);
    assert_eq!(recover["reason"], "needs_attention");
    assert_eq!(recover["action"]["reissuable"], true);
    assert!(
        !recover["refusals"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("active_execution"))
    );

    let reset = fixture.json(&[
        "auto-task",
        "reset",
        &definition.name,
        "--reason",
        "The review task closed without coverage evidence",
        "--json",
    ]);
    assert_eq!(reset["applied"], true);
    assert!(store.automation_state(&consumer).unwrap().is_none());
}

/// The auto-task defaults seeded into every workspace stay inert, declare a
/// complexity, render the workspace's own base branch, and instruct only tool
/// calls and run reads an agent can make without side effects.
#[test]
fn seeded_auto_task_defaults_are_inert_portable_and_name_only_callable_tools() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = Fixture {
        home: temp.path().join("home"),
        repo: temp.path().join("repo"),
        root: temp.path().join("state"),
        _temp: temp,
    };
    fs::create_dir_all(&fixture.home).unwrap();
    fs::create_dir_all(&fixture.repo).unwrap();
    let output = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&fixture.repo)
        .output()
        .unwrap();
    assert!(output.status.success());
    fixture
        .command(&[
            "init",
            "--non-interactive",
            "--machine-name",
            "shipped-qa",
            "--task-prefix",
            "SQ",
        ])
        .assert()
        .success();
    // A base branch no shipped asset could name by accident.
    fixture
        .command(&[
            "workspace",
            "init",
            "--name",
            "shipped-qa",
            "--base-branch",
            "trunk",
        ])
        .assert()
        .success();

    let tools: BTreeSet<String> = fixture
        .json(&["tool", "list", "--json"])
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect();
    let listed = fixture.json(&["auto-task", "list", "--format", "json"]);
    let names: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|definition| definition["name"].as_str().unwrap())
        .collect();
    assert!(!names.is_empty(), "workspace init must seed the defaults");

    let (mut deliveries, mut preconditions, mut tool_calls, mut run_reads) = (0, 0, 0, 0);
    for name in names {
        let definition = fixture.json(&["auto-task", "show", name, "--json"]);
        assert_eq!(
            definition["enabled"], false,
            "[ORB-10549] seeding must not opt the workspace into {name}"
        );
        assert!(
            definition["template"]["complexity"].is_string(),
            "[ORB-12463] {name} must declare an explicit complexity"
        );
        if let Some(delivery) = definition["schedule"].get("deliveries_landed") {
            assert_eq!(
                delivery["branch"], "trunk",
                "{name} watches the base branch"
            );
            deliveries += 1;
        }
        if let Some(precondition) = definition
            .get("skip_if_unchanged")
            .filter(|value| !value.is_null())
        {
            assert_eq!(
                precondition["ref"], "trunk",
                "[ORB-12698] {name} compares the workspace's own base branch"
            );
            preconditions += 1;
        }

        let yaml =
            fs::read_to_string(definition["definition_source"]["path"].as_str().unwrap()).unwrap();
        for tool in tool_run_mentions(&yaml) {
            assert!(
                tools.contains(&tool),
                "[ORB-12248] {name} instructs `orbit tool run {tool}`, which `orbit tool list` \
                 does not offer"
            );
            tool_calls += 1;
        }
        for command in yaml.split('`').skip(1).step_by(2) {
            let mut words = command.split_whitespace();
            if words.next() == Some("orbit")
                && words.next() == Some("run")
                && matches!(words.next(), Some("history" | "show" | "logs" | "events"))
            {
                assert!(
                    words.any(|arg| arg == "--no-reconcile"),
                    "[ORB-12943] {name} names a run read that can reconcile stale runs: {command}"
                );
                run_reads += 1;
            }
        }
    }
    assert!(
        deliveries > 0 && preconditions > 0 && tool_calls > 0 && run_reads > 0,
        "the seeded defaults must exercise every check: {deliveries} deliveries, \
         {preconditions} preconditions, {tool_calls} tool calls, {run_reads} run reads"
    );
}

/// Every tool name immediately following an `orbit tool run ` mention.
fn tool_run_mentions(text: &str) -> Vec<String> {
    const MARKER: &str = "orbit tool run ";
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(index) = rest.find(MARKER) {
        let after = &rest[index + MARKER.len()..];
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_'))
            .unwrap_or(after.len());
        names.push(after[..end].to_string());
        rest = &after[end..];
    }
    names
}
