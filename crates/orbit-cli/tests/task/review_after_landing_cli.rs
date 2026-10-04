//! `operation.review_policy = after-landing` drives the shipped
//! `delivery-code-review` consumer and `orbit doctor` fails while it cannot
//! run [ORB-13896]: the policy once did nothing without a separate toggle,
//! and a disabled, unowned or wedged consumer looked healthy for weeks.

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

/// The `review-after-landing` doctor row and whether doctor exited zero.
fn doctor_row(fixture: &Fixture) -> (Value, bool) {
    let output = fixture.command(&["doctor", "--json"]).output().unwrap();
    let rows: Value = serde_json::from_slice(&output.stdout).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["check"] == "review-after-landing")
        .cloned()
        .unwrap_or_else(|| panic!("no review-after-landing row: {rows}"));
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

#[test]
fn after_landing_policy_mints_review_batches_through_the_disabled_consumer_with_review_crew() {
    const TEST: &str = "review_after_landing_cli::after_landing_policy_mints_review_batches_through_the_disabled_consumer_with_review_crew";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = Fixture::new();
    enable_review_crew(&fixture);
    git(&fixture, &["checkout", "-b", "fixture-delivery"]);
    let baseline = commit(&fixture, "baseline\n");
    retarget(&fixture, &trigger());
    set_policy(&fixture, "operation.review_policy", "after-landing");
    set_policy(&fixture, "operation.review_crew", REVIEW_CREW);

    let shown = fixture.json(&["auto-task", "show", CONSUMER, "--json"]);
    assert_eq!(shown["enabled"], false, "no separate toggle is flipped");
    assert_eq!(shown["enabled_by_review_policy"], true);

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
    assert_eq!(listed_consumer["enabled"], false);
    assert_eq!(listed_consumer["enabled_by_review_policy"], true);
    assert_eq!(listed_consumer["effective_enabled"], true);
    let api_show = runtime
        .run_tool_as_human("orbit.auto_task.show", json!({"name": CONSUMER}))
        .unwrap();
    assert_eq!(api_show["enabled_by_review_policy"], true);
    assert_eq!(api_show["effective_enabled"], true);
    let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
    assert!(!definition.enabled);
    evaluate_auto_task(&runtime, &definition, false, Utc::now()).unwrap();
    let consumer = consumer_key(&runtime, "auto-task", CONSUMER).unwrap();
    let store = runtime.automation_store().unwrap();
    let baselined = store
        .automation_state(&consumer)
        .unwrap()
        .expect("the policy alone baselines the consumer");
    assert_eq!(baselined.observed, baseline);

    // One direct landing past the threshold of one.
    let landed = commit(&fixture, "landed\n");
    store
        .automation_record_delivery_intent(&Delivery {
            key: format!(
                "direct:{}:fixture-delivery:fixture-run",
                baselined.repository
            ),
            repository: baselined.repository.clone(),
            branch: "fixture-delivery".into(),
            before: baseline,
            after: landed.clone(),
            commits: vec![landed.commit.clone()],
            task_ids: vec![],
            unattributed: Some(UNATTRIBUTED_NO_LANDING_TASK.into()),
            evidence_reference: "run:fixture-run:direct-landing".into(),
            evidence_digest: "fixture-digest".into(),
            landed_at: Utc::now(),
        })
        .unwrap();

    let mut minted = None;
    for _ in 0..3 {
        let diagnostic = evaluate_auto_task(&runtime, &definition, false, Utc::now()).unwrap();
        minted = diagnostic
            .state
            .and_then(|state| state.active)
            .and_then(|active| active.action_id);
        if minted.is_some() {
            break;
        }
    }
    let task_id = minted.expect("after-landing must mint a review task for the landed batch");
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

    let mut wrong_coverage = trigger();
    wrong_coverage["coverage"] = json!("integrated_qa_v1");
    retarget(&fixture, &wrong_coverage);
    assert_doctor_fails(&fixture, "instead of `landed_code_review_v1`");
    retarget(&fixture, &trigger());

    set_policy(&fixture, "operation.review_crew", "missing-crew");
    assert_doctor_fails(&fixture, "does not resolve");
    set_policy(&fixture, "operation.review_crew", REVIEW_CREW);
    assert!(
        row["message"]
            .as_str()
            .unwrap()
            .contains("last batch minted"),
        "{row}"
    );
    let config = fixture.json(&["config", "show", "--json"]);
    assert_eq!(config["review_after_landing"]["healthy"], true, "{config}");
    assert_eq!(config["review_after_landing"]["crew"], REVIEW_CREW);
    assert!(
        config["review_after_landing"]["last_batch_minted_at"].is_string(),
        "{config}"
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
    assert_eq!(doctor_row(&fixture).0["status"], "skipped");
    set_policy(&fixture, "operation.review_policy", "after-landing");

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
    assert_doctor_fails(&fixture, "is missing");

    set_policy(&fixture, "operation.review_policy", "none");
    assert_eq!(doctor_row(&fixture).0["status"], "skipped");
}
