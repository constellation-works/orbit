//! After-landing review is the shipped `delivery-code-review` auto-task's
//! own `enabled` flag [ORB-13992], and `orbit doctor` fails while an enabled
//! consumer cannot run [ORB-13896]: a disabled, unowned or wedged consumer
//! once looked healthy for weeks. The deprecated
//! `operation.review_policy = "after-landing"` still enables a consumer no
//! operator has configured, for one release.

use std::fs;

use chrono::Utc;
use orbit_core::application::automation::{consumer_key, evaluate_auto_task};
use orbit_types::workflow::automation::{
    BatchAttempt, BatchState, CoverageBatch, Delivery, SourceRevision, UNATTRIBUTED_NO_LANDING_TASK,
};
use serde_json::{Value, json};

use crate::auto_task_lifecycle_cli::git;
use crate::isolated_cli_fixture::Fixture;

const CONSUMER: &str = "delivery-code-review";
const REVIEW_CREW: &str = "sonnet";

/// Runtime writes, as well as CLI writes, belong in an isolated child. Returns
/// true inside that child; the parent asserts the child passed.
fn in_isolated_child(test: &str) -> bool {
    const CHILD: &str = "ORBIT_TEST_REVIEW_AFTER_LANDING_CHILD";
    if std::env::var(CHILD).ok().as_deref() == Some(test) {
        return true;
    }
    let home = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = command
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .output()
        .unwrap();
    orbit_common::test_env::assert_child_test_passed(
        test,
        output.status,
        &output.stdout,
        &output.stderr,
    );
    false
}

fn open_runtime(fixture: &Fixture) -> orbit_core::OrbitRuntime {
    use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
    use orbit_core::ActorIdentity;

    let roots = RegisteredRuntimeFactory::resolve_roots_for_cwd(&fixture.repo, Some(&fixture.root))
        .unwrap();
    assert!(roots.global_root.starts_with(fixture._temp.path()));
    RegisteredRuntimeFactory::open_resolved_roots(roots)
        .unwrap()
        .with_actor(ActorIdentity::human("fixture"))
}

fn set_policy(fixture: &Fixture, key: &str, value: &str) {
    fixture
        .command(&["config", "set", "--global", key, value])
        .assert()
        .success();
}

/// Provider discovery at `orbit init` is host-dependent; these tests need a
/// configured review crew regardless of which provider CLIs are installed.
fn enable_review_crew(fixture: &Fixture) {
    set_policy(fixture, "crews.sonnet.enabled", "true");
    set_policy(fixture, "workflow.default_crew", REVIEW_CREW);
    set_policy(fixture, "workflow.system_crew", REVIEW_CREW);
}

/// Toggle the shipped consumer, as an operator switching after-landing
/// review does.
fn toggle(fixture: &Fixture, state: &str) {
    fixture
        .command(&["auto-task", "toggle", CONSUMER, state])
        .assert()
        .success();
}

/// Write a deprecated config key the CLI no longer sets, as an existing
/// global `config.toml` still carries it.
fn append_global_config(fixture: &Fixture, toml: &str) {
    use std::io::Write as _;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(fixture.root.join("config.toml"))
        .unwrap();
    file.write_all(toml.as_bytes()).unwrap();
}

/// Land one direct delivery past the trigger's threshold of one and
/// evaluate the consumer until it settles; the review task it minted, if any.
fn land_and_evaluate(
    fixture: &Fixture,
    runtime: &orbit_core::OrbitRuntime,
    content: &str,
) -> Option<String> {
    let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
    evaluate_auto_task(runtime, &definition, false, Utc::now()).unwrap();
    let consumer = consumer_key(runtime, "auto-task", CONSUMER).unwrap();
    let store = runtime.automation_store().unwrap();
    let before = store
        .automation_state(&consumer)
        .unwrap()
        .map(|state| (state.repository.clone(), state.observed.clone()));
    let landed = commit(fixture, content);
    if let Some((repository, before)) = &before {
        store
            .automation_record_delivery_intent(&Delivery {
                key: format!("direct:{repository}:fixture-delivery:{}", landed.commit),
                repository: repository.clone(),
                branch: "fixture-delivery".into(),
                before: before.clone(),
                after: landed.clone(),
                commits: vec![landed.commit.clone()],
                task_ids: vec![],
                unattributed: Some(UNATTRIBUTED_NO_LANDING_TASK.into()),
                evidence_reference: format!("run:fixture-run:{}", landed.commit),
                evidence_digest: "fixture-digest".into(),
                landed_at: Utc::now(),
            })
            .unwrap();
    }
    for _ in 0..3 {
        let diagnostic = evaluate_auto_task(runtime, &definition, false, Utc::now()).unwrap();
        if let Some(minted) = diagnostic
            .state
            .and_then(|state| state.active)
            .and_then(|active| active.action_id)
        {
            return Some(minted);
        }
    }
    None
}

/// Point the shipped consumer at `trigger` without touching its `enabled`.
fn retarget(fixture: &Fixture, trigger: &Value) {
    fixture.json(&[
        "auto-task",
        "update",
        CONSUMER,
        "--deliveries-landed",
        &trigger.to_string(),
        "--json",
    ]);
}

/// A persisted definition may still name the retired coverage. Add and schedule
/// update refuse to select it, so the doctor case plants the bytes the loader
/// still decodes.
fn plant_retired_coverage(fixture: &Fixture) {
    let shown = fixture.json(&["auto-task", "show", CONSUMER, "--json"]);
    let path = shown["definition_source"]["path"]
        .as_str()
        .expect("definition path");
    let current = fs::read_to_string(path).unwrap();
    let planted = current
        .replace(
            "coverage: landed_code_review_v1",
            "coverage: integrated_qa_v1",
        )
        .replace(
            "coverage: \"landed_code_review_v1\"",
            "coverage: integrated_qa_v1",
        );
    assert!(
        planted.contains("coverage: integrated_qa_v1") && planted != current,
        "the persisted consumer must still be loadable with the retired coverage"
    );
    fs::write(path, planted).unwrap();
}

fn trigger() -> Value {
    json!({"branch":"fixture-delivery","threshold":1,"max_wait_minutes":60,"coverage":"landed_code_review_v1","max_items":20,"retries":1})
}

fn commit(fixture: &Fixture, content: &str) -> SourceRevision {
    fs::write(fixture.repo.join("fixture.txt"), content).unwrap();
    git(fixture, &["add", "fixture.txt"]);
    git(fixture, &["commit", "-m", content.trim()]);
    SourceRevision {
        commit: git(fixture, &["rev-parse", "HEAD"]),
        tree: git(fixture, &["rev-parse", "HEAD^{tree}"]),
    }
}

/// The `review` doctor row and whether doctor exited zero.
fn doctor_row(fixture: &Fixture) -> (Value, bool) {
    let output = fixture.command(&["doctor", "--json"]).output().unwrap();
    let rows: Value = serde_json::from_slice(&output.stdout).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["check"] == "review")
        .cloned()
        .unwrap_or_else(|| panic!("no review row: {rows}"));
    (row, output.status.success())
}

fn assert_doctor_fails(fixture: &Fixture, expected: &str) {
    let (row, success) = doctor_row(fixture);
    assert_eq!(row["status"], "error", "{row}");
    assert!(
        !success,
        "an unhealthy after-landing consumer must fail doctor"
    );
    let message = row["message"].as_str().unwrap();
    assert!(message.contains(expected), "{message}");
}

/// [ORB-13992] The auto-task's own flag is the after-landing switch: a
/// landed delivery mints a review batch exactly while it is enabled, whatever
/// `review.before_pr` says, and the batch carries `operation.review_crew`.
#[test]
fn after_landing_review_mints_exactly_when_the_auto_task_is_enabled() {
    const TEST: &str = "review_after_landing_cli::after_landing_review_mints_exactly_when_the_auto_task_is_enabled";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    enable_review_crew(&fixture);
    git(&fixture, &["checkout", "-b", "fixture-delivery"]);
    commit(&fixture, "baseline\n");
    retarget(&fixture, &trigger());
    set_policy(&fixture, "operation.review_crew", REVIEW_CREW);
    set_policy(&fixture, "review.before_pr", "true");

    let shown = fixture.json(&["auto-task", "show", CONSUMER, "--json"]);
    assert_eq!(shown["enabled"], false);
    assert_eq!(shown["effective_enabled"], false);
    assert!(shown.get("enabled_by_review_policy").is_none(), "{shown}");
    let runtime = open_runtime(&fixture);
    assert_eq!(
        land_and_evaluate(&fixture, &runtime, "landed while disabled\n"),
        None,
        "before_pr does not switch after-landing review on"
    );
    let config = fixture.json(&["config", "show", "--json"]);
    assert_eq!(config["review"]["before_pr"]["enabled"], true, "{config}");
    assert_eq!(
        config["review"]["after_landing"]["enabled"], false,
        "{config}"
    );

    toggle(&fixture, "on");
    let runtime = open_runtime(&fixture);
    let listed = runtime
        .run_tool_as_human("orbit.auto_task.list", json!({}))
        .unwrap();
    let listed_consumer = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|definition| definition["name"] == CONSUMER)
        .unwrap();
    assert_eq!(listed_consumer["enabled"], true);
    assert_eq!(listed_consumer["effective_enabled"], true);
    assert!(listed_consumer.get("enabled_by_review_policy").is_none());
    let task_id = land_and_evaluate(&fixture, &runtime, "landed while enabled\n")
        .expect("an enabled consumer mints a review task for the landed batch");
    let task = fixture.json(&["task", "show", &task_id, "--json"]);
    assert_eq!(
        task["crew"], REVIEW_CREW,
        "operation.review_crew reviews after-landing batches: {task}"
    );
    assert!(
        task["tags"]
            .as_array()
            .unwrap()
            .contains(&json!(format!("auto-task:{CONSUMER}")))
    );

    let (row, _) = doctor_row(&fixture);
    assert_eq!(row["status"], "ok", "{row}");
    let message = row["message"].as_str().unwrap();
    assert!(message.contains("last batch minted"), "{row}");
    let config = fixture.json(&["config", "show", "--json"]);
    let after_landing = &config["review"]["after_landing"];
    assert_eq!(after_landing["enabled"], true, "{config}");
    assert_eq!(after_landing["health"]["crew"], REVIEW_CREW);
    assert!(
        after_landing["health"]["last_batch_minted_at"].is_string(),
        "{config}"
    );
    assert!(after_landing["next_batch_due"].is_string(), "{config}");
    assert_eq!(config["review"]["healthy"], true, "{config}");

    plant_retired_coverage(&fixture);
    assert_doctor_fails(&fixture, "instead of `landed_code_review_v1`");
    retarget(&fixture, &trigger());

    set_policy(&fixture, "operation.review_crew", "missing-crew");
    assert_doctor_fails(&fixture, "does not resolve");
}

/// [ORB-13992] The deprecated `operation.review_policy = "after-landing"`
/// keeps reviewing through a consumer no operator has configured, and stops
/// counting once an operator toggles the auto-task themselves.
#[test]
fn deprecated_after_landing_policy_enables_only_an_unconfigured_consumer() {
    const TEST: &str = "review_after_landing_cli::deprecated_after_landing_policy_enables_only_an_unconfigured_consumer";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    enable_review_crew(&fixture);
    append_global_config(
        &fixture,
        "\n[operation]\nreview_policy = \"after-landing\"\n",
    );

    let shown = fixture.json(&["auto-task", "show", CONSUMER, "--json"]);
    assert_eq!(shown["enabled"], false, "the seed is not rewritten");
    assert_eq!(shown["effective_enabled"], true, "{shown}");
    let config = fixture.json(&["config", "show", "--json"]);
    assert_eq!(
        config["review"]["after_landing"]["enabled"], true,
        "{config}"
    );
    assert!(
        config["review"]["after_landing"]["source"]
            .as_str()
            .unwrap()
            .contains("deprecated operation.review_policy"),
        "{config}"
    );
    assert_eq!(config["review"]["before_pr"]["enabled"], false, "{config}");

    toggle(&fixture, "off");
    let shown = fixture.json(&["auto-task", "show", CONSUMER, "--json"]);
    assert_eq!(
        shown["effective_enabled"], false,
        "an operator's toggle is explicit configuration the deprecated key no longer overrides"
    );
}

#[test]
fn doctor_fails_while_the_after_landing_consumer_cannot_review_landed_work() {
    const TEST: &str = "review_after_landing_cli::doctor_fails_while_the_after_landing_consumer_cannot_review_landed_work";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    enable_review_crew(&fixture);
    let (row, _) = doctor_row(&fixture);
    assert_eq!(row["status"], "ok", "both switches off is healthy: {row}");
    toggle(&fixture, "on");

    // The shipped consumer watches the base branch, which has no commit yet.
    assert_doctor_fails(&fixture, "does not resolve");

    git(&fixture, &["checkout", "-b", "fixture-delivery"]);
    commit(&fixture, "baseline\n");
    retarget(&fixture, &trigger());
    let (row, _) = doctor_row(&fixture);
    assert_eq!(row["status"], "ok", "{row}");

    let mut elsewhere = trigger();
    elsewhere["owner_machine"] = json!("hm_elsewhere");
    retarget(&fixture, &elsewhere);
    assert_doctor_fails(&fixture, "owned_elsewhere");
    retarget(&fixture, &trigger());

    // An admitted review task that closed without coverage wedges it.
    let runtime = open_runtime(&fixture);
    let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
    evaluate_auto_task(&runtime, &definition, false, Utc::now()).unwrap();
    let consumer = consumer_key(&runtime, "auto-task", CONSUMER).unwrap();
    let store = runtime.automation_store().unwrap();
    let baselined = store.automation_state(&consumer).unwrap().unwrap();
    let landed = commit(&fixture, "landed\n");
    let action_id = fixture.json(&[
        "task",
        "add",
        "--title",
        "Review the frozen batch",
        "--complexity",
        "low",
        "--acceptance-criteria",
        "Attach coverage evidence",
        "--json",
    ])["id"]
        .as_str()
        .unwrap()
        .to_string();
    let now = Utc::now();
    let mut admitted = baselined.clone();
    admitted.generation += 1;
    admitted.observed = landed.clone();
    admitted.pending_commits = vec![landed.commit.clone()];
    admitted.active = Some(BatchAttempt {
        batch: CoverageBatch {
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
        },
        input_digest: "fixture-input".into(),
        attempt: 1,
        action_key: "automation:fixture-batch:1".into(),
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
    assert_eq!(doctor_row(&fixture).0["status"], "ok", "an open action");
    fixture.json(&[
        "task", "update", &action_id, "--status", "rejected", "--force", "--json",
    ]);
    assert_doctor_fails(&fixture, "wedged");

    fixture.json(&[
        "auto-task",
        "delete",
        CONSUMER,
        "--reason",
        "Disposable opt-out",
        "--json",
    ]);
    let (row, _) = doctor_row(&fixture);
    assert_eq!(row["status"], "ok", "deleting the consumer opts out: {row}");
    // The deprecated policy still asks for after-landing review, which a
    // deleted consumer cannot give.
    append_global_config(
        &fixture,
        "\n[operation]\nreview_policy = \"after-landing\"\n",
    );
    assert_doctor_fails(&fixture, "is missing");

    fixture.json(&["auto-task", "restore", CONSUMER, "--json"]);
    toggle(&fixture, "off");
    assert_eq!(doctor_row(&fixture).0["status"], "ok");

    set_policy(&fixture, "review.before_pr", "true");
    set_policy(&fixture, "operation.review_crew", "missing-crew");
    assert_doctor_fails(&fixture, "does not resolve");
}
