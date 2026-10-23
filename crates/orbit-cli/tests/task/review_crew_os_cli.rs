//! After-landing review crews and the review task's `os:` tags: a pool member
//! whose executor cannot run under the `delivery-code-review` definition's
//! `os:` tags is never drawn, and a pool with no member left holds the
//! claimed batch as one visible stall that clears itself once configuration
//! admits a crew. A batch an earlier mint failure left claimed with no action
//! is minted by the next tick.

use std::fs;
use std::io::Write as _;

use chrono::Utc;
use orbit_core::application::automation::{consumer_key, evaluate_auto_task};
use orbit_types::workflow::automation::BatchState;
use serde_json::{Value, json};

use crate::auto_task_lifecycle_cli::git;
use crate::isolated_cli_fixture::Fixture;
use crate::review_after_landing_cli::{
    commit, enable_review_crew, in_isolated_child, land_and_evaluate, open_runtime, retarget,
    set_policy, toggle, trigger,
};

const CONSUMER: &str = "delivery-code-review";
const POOL_KEY: &str = "workflow.purpose_crews.review";

/// Two crews on the shipped `muse` executor, which declares `host_os: macos`.
const MUSE_CREWS: &str = r#"
[crews.muse-wonder]
model = "muse-spark-1.3"
provider = "muse"

[crews.muse-calm]
model = "muse-spark-1.3"
provider = "muse"
"#;

/// A workspace whose after-landing consumer mints review tasks tagged
/// `os:linux`, with the muse crews defined and `haiku` and `sonnet` enabled.
fn linux_tagged_consumer() -> Fixture {
    let fixture = Fixture::new();
    enable_review_crew(&fixture);
    set_policy(&fixture, "crews.haiku.enabled", "true");
    fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(fixture.root.join("config.toml"))
        .unwrap()
        .write_all(MUSE_CREWS.as_bytes())
        .unwrap();
    fixture.json(&[
        "auto-task",
        "update",
        CONSUMER,
        "--tag",
        "delivery-code-review",
        "--tag",
        "no-diff-expected",
        "--tag",
        "os:linux",
        "--json",
    ]);
    git(&fixture, &["checkout", "-b", "fixture-delivery"]);
    commit(&fixture, "baseline\n");
    retarget(&fixture, &trigger());
    toggle(&fixture, "on");
    fixture
}

/// Put the consumer's batch back in the shape a failed mint leaves: claimed
/// under `action_key`, with no action.
fn reclaim(runtime: &orbit_core::OrbitRuntime, action_key: &str) {
    let consumer = consumer_key(runtime, "auto-task", CONSUMER).unwrap();
    let store = runtime.automation_store().unwrap();
    let before = store.automation_state(&consumer).unwrap().unwrap();
    let mut claimed = before.clone();
    claimed.generation += 1;
    let attempt = claimed.active.as_mut().expect("a batch is active");
    attempt.action_key = action_key.to_string();
    attempt.action_id = None;
    attempt.state = BatchState::Claimed;
    assert!(store.automation_commit(&before, &claimed, None).unwrap());
}

/// Evaluate once, which must not fail, and return the crew of the review
/// task the resumed claim minted.
fn minted_crew(fixture: &Fixture, runtime: &orbit_core::OrbitRuntime) -> String {
    let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
    let diagnostic = evaluate_auto_task(runtime, &definition, false, Utc::now())
        .expect("a resumed claim evaluates without an error");
    let task_id = diagnostic
        .state
        .and_then(|state| state.active)
        .and_then(|active| active.action_id)
        .unwrap_or_else(|| panic!("the resumed claim mints: {}", diagnostic.reason));
    let task = fixture.json(&["task", "show", &task_id, "--json"]);
    assert_eq!(task["crew_source"], POOL_KEY, "{task}");
    task["crew"].as_str().unwrap().to_string()
}

/// [ORB-17104] The owner's pool drew `muse-wonder` for a batch whose review
/// task is tagged `os:linux`, and every retry of that batch drew it again, so
/// its mint failed on every tick. Weighted far above the other members, it is
/// still never drawn, for any batch, and a batch left claimed with no action
/// by such a failure is minted on the next tick.
#[test]
fn a_review_crew_no_host_can_run_under_the_tags_is_never_drawn() {
    const TEST: &str =
        "review_crew_os_cli::a_review_crew_no_host_can_run_under_the_tags_is_never_drawn";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = linux_tagged_consumer();
    set_policy(
        &fixture,
        "workflow.purpose_crews.review",
        r#"["muse-wonder:1000", "sonnet:1", "haiku:1"]"#,
    );
    let runtime = open_runtime(&fixture);
    let first = land_and_evaluate(&fixture, &runtime, "landed\n")
        .expect("a pool with eligible members mints the batch");
    let task = fixture.json(&["task", "show", &first, "--json"]);
    assert_ne!(task["crew"], "muse-wonder", "{task}");

    let mut drawn = std::collections::BTreeSet::new();
    for batch in 0..12 {
        let key = format!("automation:fixture-batch-{batch}:1");
        reclaim(&runtime, &key);
        let crew = minted_crew(&fixture, &runtime);
        assert!(
            crew == "sonnet" || crew == "haiku",
            "batch {key} carries a crew a linux host can run, not {crew}"
        );

        // The same batch drawn again, once its first task is gone, carries
        // the same crew.
        let consumer = consumer_key(&runtime, "auto-task", CONSUMER).unwrap();
        let minted = runtime
            .automation_store()
            .unwrap()
            .automation_state(&consumer)
            .unwrap()
            .and_then(|state| state.active)
            .and_then(|active| active.action_id)
            .unwrap();
        runtime.delete_task(&minted).unwrap();
        reclaim(&runtime, &key);
        assert_eq!(
            minted_crew(&fixture, &runtime),
            crew,
            "a retried draw for {key} is deterministic"
        );
        drawn.insert(crew);
    }
    assert_eq!(
        drawn.len(),
        2,
        "the draw still spreads over every eligible member: {drawn:?}"
    );
}

/// [ORB-17104] With every pool member excluded by the tags, ticks report one
/// typed stall instead of failing, every read surface names the excluded
/// crews, and changing the pool or the tags mints the held batch with no
/// operator recovery. The definition's own crew never replaces the pool, and
/// `update` and `show` say so.
#[test]
fn a_pool_no_host_can_run_holds_the_batch_until_configuration_admits_a_crew() {
    const TEST: &str = "review_crew_os_cli::a_pool_no_host_can_run_holds_the_batch_until_configuration_admits_a_crew";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = linux_tagged_consumer();
    set_policy(
        &fixture,
        "workflow.purpose_crews.review",
        r#"["muse-wonder", "muse-calm"]"#,
    );

    // The definition names its own crew; the pool still decides.
    let updated = fixture.json(&["auto-task", "update", CONSUMER, "--crew", "haiku", "--json"]);
    let warnings = updated["warnings"].as_array().cloned().unwrap_or_default();
    assert!(
        warnings
            .iter()
            .any(|warning| warning.as_str().is_some_and(|text| text.contains(POOL_KEY))),
        "update says the pool replaces the definition's crew: {updated}"
    );
    let text = fixture
        .command(&["auto-task", "update", CONSUMER, "--crew", "haiku"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&text.get_output().stdout).into_owned();
    assert!(stdout.contains(POOL_KEY), "{stdout}");

    let runtime = open_runtime(&fixture);
    assert_eq!(
        land_and_evaluate(&fixture, &runtime, "landed\n"),
        None,
        "no review task is minted with a crew no host can run"
    );
    let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
    for _ in 0..2 {
        let held = evaluate_auto_task(&runtime, &definition, false, Utc::now())
            .expect("a held batch does not fail the tick");
        assert_eq!(held.reason, "stalled: crew_os_ineligible");
    }

    let shown = fixture.json(&["auto-task", "show", CONSUMER, "--json"]);
    assert!(
        shown["crew_overridden"]
            .as_str()
            .is_some_and(|note| note.contains(POOL_KEY)),
        "{shown}"
    );
    assert_eq!(shown["automation"]["reason"], "stalled: crew_os_ineligible");
    let stall = &shown["automation"]["state"]["stall"];
    assert_eq!(stall["reason"], "crew_os_ineligible", "{shown}");
    let hold = &stall["crew_hold"];
    assert_eq!(hold["definition"], CONSUMER);
    assert_eq!(hold["pool_key"], POOL_KEY);
    assert_eq!(hold["os_tags"], json!(["os:linux"]));
    let mut excluded = hold["excluded"].as_array().unwrap().clone();
    excluded.sort_by_key(|crew| crew["crew"].as_str().unwrap().to_string());
    assert_eq!(
        Value::Array(excluded),
        json!([
            {"crew": "muse-calm", "executor": "muse", "host_os": ["macos"]},
            {"crew": "muse-wonder", "executor": "muse", "host_os": ["macos"]},
        ])
    );
    let text = fixture
        .command(&["auto-task", "show", CONSUMER])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&text.get_output().stdout).into_owned();
    let stalled = stdout
        .lines()
        .find(|line| line.trim_start().starts_with("stalled:"))
        .unwrap_or_else(|| panic!("show prints the stall: {stdout}"));
    for named in ["muse-wonder", "muse-calm", "macos", "os:linux", POOL_KEY] {
        assert!(stalled.contains(named), "{named} in {stalled}");
    }

    let config = fixture.json(&["config", "show", "--json"]);
    let health = &config["review"]["after_landing"]["health"];
    assert!(
        health["stall"]
            .as_str()
            .is_some_and(|stall| stall.contains("muse-wonder") && stall.contains("os:linux")),
        "{config}"
    );
    assert!(
        health["problems"]
            .as_array()
            .is_some_and(|problems| problems
                .iter()
                .any(|problem| problem.as_str().unwrap().contains("muse-calm"))),
        "{config}"
    );

    // Adding a crew a linux host can run to the pool mints the held batch on
    // the next tick.
    set_policy(
        &fixture,
        "workflow.purpose_crews.review",
        r#"["muse-wonder", "sonnet"]"#,
    );
    let runtime = open_runtime(&fixture);
    assert_eq!(
        minted_crew(&fixture, &runtime),
        "sonnet",
        "the pool, not haiku"
    );
    let consumer = consumer_key(&runtime, "auto-task", CONSUMER).unwrap();
    let store = runtime.automation_store().unwrap();
    assert!(
        store
            .automation_state(&consumer)
            .unwrap()
            .unwrap()
            .stall
            .is_none()
    );

    // Held again, retagging the definition so the muse crews can run is
    // adopted over the never-minted claim, which is minted in the same tick.
    set_policy(
        &fixture,
        "workflow.purpose_crews.review",
        r#"["muse-wonder", "muse-calm"]"#,
    );
    let runtime = open_runtime(&fixture);
    reclaim(&runtime, "automation:fixture-held:1");
    let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
    let held = evaluate_auto_task(&runtime, &definition, false, Utc::now()).unwrap();
    assert_eq!(held.reason, "stalled: crew_os_ineligible");
    fixture.json(&[
        "auto-task",
        "update",
        CONSUMER,
        "--tag",
        "delivery-code-review",
        "--tag",
        "no-diff-expected",
        "--tag",
        "os:macos",
        "--json",
    ]);
    let runtime = open_runtime(&fixture);
    let crew = minted_crew(&fixture, &runtime);
    assert!(crew.starts_with("muse-"), "a pool member: {crew}");
    assert!(
        store
            .automation_state(&consumer)
            .unwrap()
            .unwrap()
            .stall
            .is_none()
    );
}

/// [ORB-17129] A parked compatible crew cannot admit a batch or clear an
/// existing OS hold. Adding a selectable compatible crew resumes that same
/// batch while the parked member stays out of the draw.
#[test]
fn parked_compatible_crews_hold_the_batch_until_a_positive_member_is_added() {
    const TEST: &str = "review_crew_os_cli::parked_compatible_crews_hold_the_batch_until_a_positive_member_is_added";
    if !in_isolated_child(TEST) {
        return;
    }

    let fixture = linux_tagged_consumer();
    set_policy(&fixture, POOL_KEY, r#"["muse-wonder:1", "sonnet:0"]"#);
    let runtime = open_runtime(&fixture);
    assert_eq!(
        land_and_evaluate(&fixture, &runtime, "landed\n"),
        None,
        "a parked compatible crew does not mint a review"
    );
    let initial = fixture.json(&["auto-task", "show", CONSUMER, "--json"]);
    assert_eq!(
        initial["automation"]["reason"],
        "stalled: crew_os_ineligible"
    );
    let active = initial["automation"]["state"]["active"].clone();
    assert_eq!(active["state"], "claimed", "{initial}");
    assert!(active["action_id"].is_null(), "{initial}");
    let stall = initial["automation"]["state"]["stall"].clone();
    assert_eq!(
        stall["crew_hold"]["excluded"],
        json!([{"crew": "muse-wonder", "executor": "muse", "host_os": ["macos"]}]),
        "only selectable crews contribute to the OS exclusion: {initial}"
    );

    // Exercise both repeated ticks and replacing an already-held pool with
    // one that adds a parked, OS-compatible crew.
    for pool in [
        r#"["muse-wonder:1", "sonnet:0"]"#,
        r#"["muse-wonder:1"]"#,
        r#"["muse-wonder:1", "sonnet:0"]"#,
    ] {
        set_policy(&fixture, POOL_KEY, pool);
        let runtime = open_runtime(&fixture);
        let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
        let held = evaluate_auto_task(&runtime, &definition, false, Utc::now())
            .expect("parked crews neither fail a tick nor clear an existing OS hold");
        assert_eq!(held.reason, "stalled: crew_os_ineligible");
        let shown = fixture.json(&["auto-task", "show", CONSUMER, "--json"]);
        assert_eq!(shown["automation"]["state"]["active"], active);
        assert_eq!(shown["automation"]["state"]["stall"], stall);
    }
    let config = fixture.json(&["config", "show", "--json"]);
    assert!(
        config["review"]["after_landing"]["health"]["stall"]
            .as_str()
            .is_some_and(|text| text.contains("crew_os_ineligible")),
        "the hold is visible in review health: {config}"
    );

    set_policy(
        &fixture,
        POOL_KEY,
        r#"["muse-wonder:1", "sonnet:0", "haiku:1"]"#,
    );
    let runtime = open_runtime(&fixture);
    assert_eq!(minted_crew(&fixture, &runtime), "haiku");
    let resumed = fixture.json(&["auto-task", "show", CONSUMER, "--json"]);
    let state = &resumed["automation"]["state"];
    assert!(state["stall"].is_null(), "{resumed}");
    assert_eq!(state["active"]["batch"], active["batch"]);
    assert_eq!(state["active"]["action_key"], active["action_key"]);
    assert!(state["active"]["action_id"].is_string(), "{resumed}");
}
