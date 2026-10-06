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
    let mut trigger = serde_json::json!({"branch":"fixture-delivery","threshold":1,"max_wait_minutes":60,"coverage":"landed_code_review_v1","max_items":20,"retries":0});
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

#[test]
fn auto_task_cli_delete_failing_after_consumer_reset_keeps_definition_and_cursor() {
    use orbit_common::security::release::sha256_hex;
    use orbit_core::application::auto_tasks::{
        AutoTaskCursor, cursor_state_path, load_cursor_state,
    };
    use orbit_core::application::automation::consumer_key;
    use orbit_types::workflow::AutoTaskPendingClaim;

    let fixture = Fixture::new();
    let trigger = serde_json::json!({"branch":"fixture-delivery","threshold":1,"max_wait_minutes":60,"coverage":"landed_code_review_v1","max_items":20,"retries":0});
    let (runtime, definition) = baselined_delivery_consumer(&fixture, &trigger);
    let name = definition.name.as_str();
    let consumer = consumer_key(&runtime, "auto-task", name).unwrap();
    let store = runtime.automation_store().unwrap();
    assert!(store.automation_state(&consumer).unwrap().is_some());

    // A pin whose ref lock is held cannot be deleted, so teardown fails after
    // the consumer reset has already applied.
    let pin = format!(
        "refs/orbit/automation/{}/fixture-pin",
        sha256_hex(consumer.as_bytes())
    );
    git(&fixture, &["update-ref", &pin, "HEAD"]);
    let ref_lock = fixture.repo.join(git(
        &fixture,
        &["rev-parse", "--git-path", &format!("{pin}.lock")],
    ));
    fs::write(&ref_lock, "").unwrap();

    let cursor_path = cursor_state_path(&runtime.paths().state_dir);
    let mut cursors = load_cursor_state(&cursor_path).unwrap();
    cursors.definitions.insert(
        name.to_string(),
        AutoTaskCursor {
            baseline_at: "2026-01-01T00:00:00Z".to_string(),
            last_slot: None,
            last_fired_at: None,
            last_task_id: None,
            pending: Some(AutoTaskPendingClaim {
                slot: "2026-01-01T01:00:00Z".to_string(),
                task_id: None,
            }),
            last_skip: None,
        },
    );
    fs::write(
        &cursor_path,
        serde_json::to_string_pretty(&cursors).unwrap(),
    )
    .unwrap();
    // `automation` reports the consumer state, which the reset drops.
    let show_definition = || {
        let mut shown = fixture.json(&["auto-task", "show", name, "--json"]);
        shown.as_object_mut().unwrap().remove("automation");
        shown
    };
    let definition_before = show_definition();

    fixture
        .command(&[
            "auto-task",
            "delete",
            name,
            "--reason",
            "Disposable failing delete",
            "--json",
        ])
        .assert()
        .failure();
    assert!(
        store.automation_state(&consumer).unwrap().is_none(),
        "the failure must come after teardown applied the consumer reset"
    );
    assert_eq!(
        load_cursor_state(&cursor_path).unwrap(),
        cursors,
        "a failed delete must leave the scheduler cursor, pending claim included"
    );
    assert_eq!(show_definition(), definition_before);

    fs::remove_file(&ref_lock).unwrap();
    let removed = fixture.json(&[
        "auto-task",
        "delete",
        name,
        "--reason",
        "Disposable delete",
        "--json",
    ]);
    assert_eq!(removed["cursor_removed"], true);
    assert_eq!(
        removed["consumer"]["released_refs"],
        serde_json::json!([pin])
    );
    assert!(
        !load_cursor_state(&cursor_path)
            .unwrap()
            .definitions
            .contains_key(name)
    );
    assert!(git(&fixture, &["for-each-ref", &pin]).is_empty());
}

/// Run one git command in the fixture repository, isolated from the caller's
/// configuration, and return its trimmed stdout.
pub(crate) fn git(fixture: &Fixture, args: &[&str]) -> String {
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

#[test]
fn auto_task_add_and_schedule_update_reject_retired_integrated_qa_coverage() {
    let fixture = Fixture::new();
    let retired = r#"{"branch":"fixture-delivery","threshold":1,"max_wait_minutes":60,"coverage":"integrated_qa_v1","max_items":20,"retries":0}"#;
    let review = r#"{"branch":"fixture-delivery","threshold":1,"max_wait_minutes":60,"coverage":"landed_code_review_v1","max_items":20,"retries":0}"#;
    fixture
        .command(&[
            "auto-task",
            "add",
            "--name",
            "retired-coverage",
            "--deliveries-landed",
            retired,
            "--title",
            "Must not persist",
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "coverage `integrated_qa_v1` is retired",
        ));
    assert!(
        !fixture
            .repo
            .join(".orbit/auto_tasks/retired-coverage.yaml")
            .exists()
    );
    fixture.json(&[
        "auto-task",
        "add",
        "--name",
        "review-coverage",
        "--deliveries-landed",
        review,
        "--title",
        "Review coverage still adds",
        "--json",
    ]);
    fixture
        .command(&[
            "auto-task",
            "update",
            "review-coverage",
            "--deliveries-landed",
            retired,
            "--json",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "coverage `integrated_qa_v1` is retired",
        ));
    let shown = fixture.json(&["auto-task", "show", "review-coverage", "--json"]);
    assert_eq!(
        shown["schedule"]["deliveries_landed"]["coverage"], "landed_code_review_v1",
        "a refused schedule update must leave the review consumer unchanged"
    );
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
    // A configured origin is observed by fetching it. Publish the branch first
    // so the baseline is that remote head rather than a fetch failure.
    publish_origin_if_configured(fixture);
    evaluate_auto_task(&runtime, &definition, false, Utc::now()).unwrap();
    (runtime, definition)
}

/// Mirror `origin` at a bare repo beside the fixture and push `HEAD` there.
///
/// The origin URL is left as configured. Fetch and push follow
/// `url.<bare>.insteadOf`, which is how a GitHub identity stays intact while
/// the objects live on disk.
pub(crate) fn publish_origin_if_configured(fixture: &Fixture) {
    let remotes = git(fixture, &["remote"]);
    if !remotes.split_whitespace().any(|name| name == "origin") {
        return;
    }
    let url = git(fixture, &["config", "--get", "remote.origin.url"]);
    let bare = fixture.repo.with_file_name("origin.git");
    let bare_path = bare.display().to_string();
    if !bare.join("HEAD").exists() {
        git(fixture, &["init", "--bare", "-q", &bare_path]);
    }
    let branch = git(fixture, &["branch", "--show-current"]);
    git(
        fixture,
        &[
            "--git-dir",
            &bare_path,
            "symbolic-ref",
            "HEAD",
            &format!("refs/heads/{branch}"),
        ],
    );
    let key = format!("url.{bare_path}.insteadOf");
    git(fixture, &["config", &key, &url]);
    git(
        fixture,
        &["push", "-q", "origin", &format!("HEAD:refs/heads/{branch}")],
    );
}

#[test]
fn doctor_scans_all_consumer_pages_and_warns_on_a_later_read_failure() {
    use orbit_core::application::automation::{consumer_key, stalled_consumers};
    use orbit_types::workflow::automation::recovery::AutomationStall;

    // Runtime writes, as well as CLI writes, belong in an isolated child.
    const TEST: &str = "auto_task_lifecycle_cli::doctor_scans_all_consumer_pages_and_warns_on_a_later_read_failure";
    const CHILD: &str = "ORBIT_TEST_CONSUMER_SCAN_CHILD";
    if std::env::var(CHILD).ok().as_deref() != Some(TEST) {
        let home = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        orbit_common::test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        let output = command
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, TEST)
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .current_dir(home.path())
            .output()
            .unwrap();
        orbit_common::test_env::assert_child_test_passed(
            TEST,
            output.status,
            &output.stdout,
            &output.stderr,
        );
        return;
    }

    let fixture = Fixture::new();
    let trigger = serde_json::json!({"branch":"fixture-delivery","threshold":1,"max_wait_minutes":60,"coverage":"landed_code_review_v1","max_items":20,"retries":0});
    let (runtime, definition) = baselined_delivery_consumer(&fixture, &trigger);
    let consumer = consumer_key(&runtime, "auto-task", &definition.name).unwrap();
    let store = runtime.automation_store().unwrap();
    let baseline = store.automation_state(&consumer).unwrap().unwrap();
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

    // The original consumer sorts after these 200 healthy states.
    for index in 0..200 {
        let mut healthy = baseline.clone();
        healthy.consumer = consumer_key(&runtime, "auto-task", &format!("aaa-{index:03}")).unwrap();
        assert!(store.automation_initialize(&healthy).unwrap());
    }
    assert!(stalled_consumers(&runtime).unwrap().is_empty());
    assert_eq!(doctor_row()["status"], "ok");

    let mut stalled = baseline.clone();
    stalled.generation += 1;
    stalled.stall = Some(AutomationStall {
        reason: "history_diverged".into(),
        since: chrono::Utc::now(),
        escalated_at: None,
        friction_id: None,
        divergence: None,
    });
    assert!(store.automation_stall(&baseline, &stalled).unwrap());
    let listed = stalled_consumers(&runtime).unwrap();
    assert_eq!(
        listed.len(),
        1,
        "a stall beyond two full pages must be listed"
    );
    assert_eq!(listed[0].consumer, consumer);
    let row = doctor_row();
    assert_eq!(row["status"], "warning");
    assert!(row["message"].as_str().unwrap().contains(&definition.name));

    // A malformed later page must fail the scan rather than return the
    // healthy prefix or a partial list of stalls.
    let conn = rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
    assert_eq!(
        conn.execute(
            "UPDATE automation_consumers SET state_json='invalid JSON' WHERE consumer=?1",
            [&consumer],
        )
        .unwrap(),
        1
    );
    let error = stalled_consumers(&runtime).unwrap_err().to_string();
    let row = doctor_row();
    assert_eq!(row["status"], "warning");
    assert!(row["message"].as_str().unwrap().contains(&error));
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

/// Every frozen review delivery once carried empty `task_ids`, so a reviewer
/// could attribute a finding only from merge-commit text. A PR landing now
/// names the tasks whose landing record holds the PR — one task, every member
/// of a bundle — and a PR no task records says so explicitly.
#[cfg(unix)]
#[test]
fn provider_landings_carry_the_tasks_that_recorded_the_pull_request() {
    use std::os::unix::fs::PermissionsExt;

    const TEST: &str =
        "auto_task_lifecycle_cli::provider_landings_carry_the_tasks_that_recorded_the_pull_request";
    const CHILD: &str = "ORBIT_TEST_DELIVERY_ATTRIBUTION_CHILD";
    if std::env::var(CHILD).ok().as_deref() != Some(TEST) {
        let home = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        orbit_common::test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        let output = command
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, TEST)
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .current_dir(home.path())
            .output()
            .unwrap();
        orbit_common::test_env::assert_child_test_passed(
            TEST,
            output.status,
            &output.stdout,
            &output.stderr,
        );
        return;
    }

    const REPOSITORY: &str = "fixture-owner/fixture-repo";
    let fixture = Fixture::new();
    git(
        &fixture,
        &[
            "remote",
            "add",
            "origin",
            &format!("https://github.com/{REPOSITORY}.git"),
        ],
    );
    let trigger = serde_json::json!({"branch":"fixture-delivery","threshold":1,"max_wait_minutes":60,"coverage":"landed_code_review_v1","max_items":20,"retries":0});
    let (_runtime, definition) = baselined_delivery_consumer(&fixture, &trigger);

    // Promotion stamps the PR on every task it delivers.
    let task = |title: &str, pr: u64| {
        fixture.json(&[
            "task",
            "add",
            "--title",
            title,
            "--complexity",
            "low",
            "--acceptance-criteria",
            "Lands through a pull request",
            "--ref",
            &format!("github-pr:{pr}"),
            "--json",
        ])["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let single = task("Single-task delivery", 7);
    let mut bundle = vec![task("Bundle member one", 8), task("Bundle member two", 8)];
    bundle.sort();

    // Each PR squash-merges as one first-parent commit; the stand-in `gh`
    // answers the provider's commit-to-PR lookup from these records.
    let bin = fixture._temp.path().join("bin");
    let pulls = bin.join("pulls");
    fs::create_dir_all(&pulls).unwrap();
    for pr in [7_u64, 8, 9] {
        fs::write(
            fixture.repo.join("fixture.txt"),
            format!("landed by #{pr}\n"),
        )
        .unwrap();
        git(&fixture, &["commit", "-am", &format!("Squash-merge #{pr}")]);
        let sha = git(&fixture, &["rev-parse", "HEAD"]);
        let response = serde_json::json!([{
            "number": pr,
            "html_url": format!("https://github.com/{REPOSITORY}/pull/{pr}"),
            "merge_commit_sha": sha,
            "merged_at": "2026-10-04T00:00:00Z",
            "base": {"ref": "fixture-delivery", "repo": {"full_name": REPOSITORY}},
        }]);
        fs::write(pulls.join(format!("{sha}.json")), response.to_string()).unwrap();
    }
    let gh = bin.join("gh");
    fs::write(
        &gh,
        "#!/bin/sh\nsha=${2#*/commits/}\nexec cat \"$(dirname \"$0\")/pulls/${sha%%/*}.json\"\n",
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    publish_origin_if_configured(&fixture);
    let path = std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();

    let output = fixture
        .command(&["auto-task", "show", &definition.name, "--preview", "--json"])
        .env("PATH", path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let shown: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let pending = shown["automation"]["state"]["pending"]
        .as_array()
        .unwrap_or_else(|| panic!("no pending deliveries: {shown}"));
    let delivery = |pr: u64| {
        let key = format!("pr:{REPOSITORY}:fixture-delivery:{pr}");
        pending
            .iter()
            .find(|delivery| delivery["key"] == key.as_str())
            .unwrap_or_else(|| panic!("no delivery {key}: {shown}"))
    };

    assert_eq!(delivery(7)["task_ids"], serde_json::json!([single]));
    assert!(delivery(7).get("unattributed").is_none());
    assert_eq!(
        delivery(8)["task_ids"],
        serde_json::json!(bundle),
        "a bundle's delivery lists every member task"
    );
    assert_eq!(delivery(9)["task_ids"], serde_json::json!([]));
    assert_eq!(
        delivery(9)["unattributed"],
        orbit_types::workflow::automation::UNATTRIBUTED_NO_LANDING_TASK
    );
}
