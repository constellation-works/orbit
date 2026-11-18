//! After-landing review crews and a Muse model this host's provider account
//! was verified to refuse: a freshly minted review batch skips every pool
//! member carrying that model, a pool with none left holds the claimed batch
//! until the refusal clears, and only verified owner-local implementation
//! evidence on the same host and account can exclude a model.

use std::fs;
use std::io::Write as _;

use chrono::{Duration, Utc};
use orbit_core::OrbitRuntime;
use orbit_core::application::automation::{consumer_key, evaluate_auto_task};
use orbit_engine::RuntimeHost;
use orbit_types::workflow::{
    IMPLEMENT_ONE_STEP_ID, JobRunState, MUSE_MODEL_ACCESS_OUTPUT_KEY, MuseModelAccess,
    PipelineState, TASK_LOCAL_PIPELINE_JOB,
};
use serde_json::{Value, json};

use crate::isolated_cli_fixture::Fixture;
use crate::review_after_landing_cli::{
    commit, enable_review_crew, in_isolated_child, land_and_evaluate, open_runtime, retarget,
    set_policy, toggle, trigger,
};
use crate::review_crew_os_cli::{escalated_hold_body, minted_crew, reclaim};

const CONSUMER: &str = "delivery-code-review";
const POOL_KEY: &str = "workflow.purpose_crews.review";

/// A second crew on `muse-wonder`'s model: the same provider-model refusal
/// covers both aliases.
const ALIAS_CREWS: &str = r#"
[crews.muse-alias]
model = "{model}"
provider = "muse"
"#;

/// The seeded `muse-wonder` model and provider account.
fn wonder_access(runtime: &OrbitRuntime) -> (String, Option<String>) {
    let crew = runtime
        .resolve_crew_for_task(Some("muse-wonder"), None)
        .expect("muse-wonder resolves");
    (
        crew.assignment.model.clone(),
        crew.account().map(str::to_string),
    )
}

fn consumer_with_pool(pool: &str) -> (Fixture, String, Option<String>) {
    let fixture = Fixture::new();
    enable_review_crew(&fixture);
    set_policy(&fixture, "crews.haiku.enabled", "true");
    set_policy(&fixture, "crews.muse-wonder.enabled", "true");
    let (model, account) = wonder_access(&open_runtime(&fixture));
    fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(fixture.root.join("config.toml"))
        .unwrap()
        .write_all(ALIAS_CREWS.replace("{model}", &model).as_bytes())
        .unwrap();
    set_policy(&fixture, POOL_KEY, pool);
    let git = crate::auto_task_lifecycle_cli::git;
    git(&fixture, &["checkout", "-b", "fixture-delivery"]);
    commit(&fixture, "baseline\n");
    retarget(&fixture, &trigger());
    toggle(&fixture, "on");
    (fixture, model, account)
}

/// Record one implementation run's Muse model-access observation, as the
/// provider completion projection writes it, finished `ago` before now.
fn seed_access(
    runtime: &OrbitRuntime,
    job: &str,
    access: &(String, Option<String>),
    refused: bool,
    ago: Duration,
) {
    let at = Utc::now() - ago;
    let input = json!({"task_id": "ORB-1"});
    let run = runtime
        .insert_job_run(job, 1, at, Some(input.clone()), None)
        .unwrap();
    let mut state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), input);
    state.pipeline = json!({
        IMPLEMENT_ONE_STEP_ID: {
            MUSE_MODEL_ACCESS_OUTPUT_KEY: serde_json::to_value(MuseModelAccess {
                model: access.0.clone(),
                crew_account: access.1.clone(),
                started_at: at,
                finished_at: at,
                refused,
            })
            .unwrap(),
        },
    });
    runtime.write_run_state(&run.run_id, &state).unwrap();
    runtime
        .mark_job_run_running(&run.run_id, at, std::process::id())
        .unwrap();
    runtime
        .finalize_job_run(&run.run_id, JobRunState::Failed, at, None)
        .unwrap();
}

fn hold_of(fixture: &Fixture) -> Value {
    let shown = fixture.json(&["auto-task", "show", CONSUMER, "--json"]);
    assert_eq!(
        shown["automation"]["reason"], "stalled: crew_os_ineligible",
        "{shown}"
    );
    shown["automation"]["state"]["stall"]["crew_hold"].clone()
}

/// [ORB-25562] A different task already proved Muse refuses the model on this
/// host and account. The next review batch carries the remaining reviewer,
/// the weighted draw still spreads over what remains, and evidence that is
/// not this host's verified owner-local implementation observation excludes
/// nothing.
#[test]
fn a_verified_refusal_from_another_task_keeps_its_model_out_of_the_next_batch() {
    const TEST: &str = "review_crew_model_access_cli::a_verified_refusal_from_another_task_keeps_its_model_out_of_the_next_batch";
    if !in_isolated_child(TEST) {
        return;
    }

    let (fixture, model, account) =
        consumer_with_pool(r#"["muse-wonder:1000", "muse-alias:1000", "haiku:1", "sonnet:1"]"#);
    let access = (model, account);
    let runtime = open_runtime(&fixture);

    // Not evidence: a different host's run, a claimed follower's leaf, a
    // success, a refusal past the window, and another provider account.
    seed_access(
        &open_runtime(&fixture).with_automation_machine_identity(Some("hm_other_host".into())),
        TASK_LOCAL_PIPELINE_JOB,
        &access,
        true,
        Duration::minutes(5),
    );
    seed_access(
        &runtime,
        "task_claimed_local_pipeline",
        &access,
        true,
        Duration::minutes(5),
    );
    seed_access(
        &runtime,
        TASK_LOCAL_PIPELINE_JOB,
        &access,
        false,
        Duration::minutes(5),
    );
    seed_access(
        &runtime,
        TASK_LOCAL_PIPELINE_JOB,
        &access,
        true,
        Duration::hours(13),
    );
    let other_account = (access.0.clone(), Some("another-account".to_string()));
    if access.1 != other_account.1 {
        seed_access(
            &runtime,
            TASK_LOCAL_PIPELINE_JOB,
            &other_account,
            true,
            Duration::minutes(5),
        );
    }
    let first = land_and_evaluate(&fixture, &runtime, "landed\n")
        .expect("a pool with eligible members mints the batch");
    let task = fixture.json(&["task", "show", &first, "--json"]);
    assert!(
        task["crew"].as_str().unwrap().starts_with("muse-"),
        "nothing above excludes the model, so the 1000-weight aliases are drawn: {task}"
    );

    // The verified refusal of a different task's owner-local run.
    seed_access(
        &runtime,
        TASK_LOCAL_PIPELINE_JOB,
        &access,
        true,
        Duration::minutes(5),
    );
    let consumer = consumer_key(&runtime, "auto-task", CONSUMER).unwrap();
    let mut drawn = std::collections::BTreeSet::new();
    for batch in 0..12 {
        let key = format!("automation:fixture-batch-{batch}:1");
        reclaim(&runtime, &key);
        let crew = minted_crew(&fixture, &runtime);
        assert!(
            crew == "haiku" || crew == "sonnet",
            "batch {key} must skip both aliases of the refused model, not {crew}"
        );
        drawn.insert(crew);
        let minted = runtime
            .automation_store()
            .unwrap()
            .automation_state(&consumer)
            .unwrap()
            .and_then(|state| state.active)
            .and_then(|active| active.action_id)
            .unwrap();
        runtime.delete_task(&minted).unwrap();
    }
    assert_eq!(
        drawn.len(),
        2,
        "the weighted draw still spreads over every eligible member: {drawn:?}"
    );
}

/// [ORB-25562] With every reviewer's model refused the claimed batch is held
/// with a typed, actionable stall instead of a failed mint; the batch and its
/// cursor stay put. A newer verified success, or the refusal's expiry,
/// releases it, and a host without the refusal is unaffected.
#[test]
fn a_pool_whose_every_model_is_refused_holds_the_batch_until_the_refusal_clears() {
    const TEST: &str = "review_crew_model_access_cli::a_pool_whose_every_model_is_refused_holds_the_batch_until_the_refusal_clears";
    if !in_isolated_child(TEST) {
        return;
    }

    let (fixture, model, account) = consumer_with_pool(r#"["muse-wonder", "muse-alias"]"#);
    let access = (model.clone(), account);
    let runtime = open_runtime(&fixture);
    seed_access(
        &runtime,
        TASK_LOCAL_PIPELINE_JOB,
        &access,
        true,
        Duration::minutes(5),
    );

    assert_eq!(
        land_and_evaluate(&fixture, &runtime, "landed\n"),
        None,
        "no review task is minted with a refused model"
    );
    let held_state = |runtime: &OrbitRuntime| {
        let consumer = consumer_key(runtime, "auto-task", CONSUMER).unwrap();
        runtime
            .automation_store()
            .unwrap()
            .automation_state(&consumer)
            .unwrap()
            .unwrap()
    };
    let before = held_state(&runtime);
    let active = before.active.clone().expect("the frozen batch is retained");
    assert!(active.action_id.is_none());

    let hold = hold_of(&fixture);
    assert_eq!(hold["excluded"], json!([]), "{hold}");
    let mut refused = hold["model_refused"].as_array().unwrap().clone();
    refused.sort_by_key(|crew| crew["crew"].as_str().unwrap().to_string());
    assert_eq!(refused.len(), 2, "{hold}");
    for (crew, name) in refused.iter().zip(["muse-alias", "muse-wonder"]) {
        assert_eq!(crew["crew"], name);
        assert_eq!(crew["model"], model.as_str());
        assert!(crew["clears_at"].is_string(), "{crew}");
    }
    let text = fixture
        .command(&["auto-task", "show", CONSUMER])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&text.get_output().stdout).into_owned();
    let stalled = stdout
        .lines()
        .find(|line| line.trim_start().starts_with("stalled:"))
        .unwrap_or_else(|| panic!("show prints the stall: {stdout}"));
    assert!(
        stalled.contains(&model) && stalled.contains("expiry"),
        "{stalled}"
    );
    let config = fixture.json(&["config", "show", "--json"]);
    assert!(
        config["review"]["after_landing"]["health"]["stall"]
            .as_str()
            .is_some_and(|stall| stall.contains(&model)),
        "the hold is visible in review health: {config}"
    );

    // Ticks keep the batch and cursor exactly as they were.
    let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
    let held = evaluate_auto_task(&runtime, &definition, false, Utc::now())
        .expect("a held batch does not fail the tick");
    assert_eq!(held.reason, "stalled: crew_os_ineligible");
    let after = held_state(&runtime);
    assert_eq!(after.active, before.active);
    assert_eq!(after.observed, before.observed);
    assert_eq!(after.pending, before.pending);

    // A newer verified success of the model in the same context releases the
    // hold, and the same tick mints the held batch.
    seed_access(
        &runtime,
        TASK_LOCAL_PIPELINE_JOB,
        &access,
        false,
        Duration::seconds(1),
    );
    let crew = minted_crew(&fixture, &runtime);
    assert!(crew.starts_with("muse-"), "a pool member: {crew}");
    let released = held_state(&runtime);
    assert!(released.stall.is_none(), "{released:?}");
    let minted = released.active.expect("the batch is minted");
    assert_eq!(minted.action_key, active.action_key);
    assert_eq!(minted.batch, active.batch);
}

/// [ORB-25584] A hold that only a refused model causes, once it escalates past
/// the window, is filed with the model-access way out and names no `os:` tag
/// remediation: editing tags cannot clear a refusal.
#[test]
fn an_escalated_model_refusal_hold_names_model_access_not_os_tags() {
    const TEST: &str = "review_crew_model_access_cli::an_escalated_model_refusal_hold_names_model_access_not_os_tags";
    if !in_isolated_child(TEST) {
        return;
    }

    let (fixture, model, account) = consumer_with_pool(r#"["muse-wonder", "muse-alias"]"#);
    let access = (model.clone(), account);
    let runtime = open_runtime(&fixture);
    seed_access(
        &runtime,
        TASK_LOCAL_PIPELINE_JOB,
        &access,
        true,
        Duration::minutes(5),
    );
    assert_eq!(
        land_and_evaluate(&fixture, &runtime, "landed\n"),
        None,
        "no review task is minted with a refused model"
    );
    let hold = hold_of(&fixture);
    assert_eq!(hold["excluded"], json!([]), "{hold}");

    let body = escalated_hold_body(&fixture, &runtime);
    let refused = hold["model_refused"].as_array().unwrap();
    assert_eq!(refused.len(), 2, "{hold}");
    for crew in refused {
        let name = crew["crew"].as_str().unwrap();
        let clears_at = crew["clears_at"].as_str().unwrap();
        let until = chrono::DateTime::parse_from_rfc3339(clears_at)
            .unwrap()
            .to_rfc3339();
        assert!(
            body.contains(name) && body.contains(&model) && body.contains(&until),
            "the escalation names `{name}` with its model and when it lapses: {body}"
        );
    }
    assert!(
        body.contains("whose model is not refused")
            && body.contains("a verified success of its model"),
        "the escalation offers model-access remediation: {body}"
    );
    assert!(
        !body.contains("`os:` tags"),
        "a refused model is not cleared by editing os tags: {body}"
    );
    assert!(
        !body.contains("executor can run on one of"),
        "a refused model is not cleared by an OS-matching crew: {body}"
    );
}

/// Pin `executor` to one host OS, the same way the OS-hold fixture constrains
/// `muse`. The shipped assets omit `host_os`, so a crew on this executor is
/// eligible on every OS until this runs.
fn pin_executor_host_os(fixture: &Fixture, executor: &str, host: orbit_types::task::HostOs) {
    let runtime = open_runtime(fixture);
    let mut definition = runtime
        .list_executor_defs()
        .expect("list executors")
        .into_iter()
        .find(|definition| definition.name == executor)
        .unwrap_or_else(|| panic!("{executor} seeded"));
    definition.host_os = Some(std::collections::BTreeSet::from([host]));
    runtime
        .upsert_executor_def(&definition)
        .unwrap_or_else(|error| panic!("pin {executor} host_os: {error}"));
}

/// [ORB-25599] A Linux-tagged hold whose only OS-eligible reviewer is
/// model-refused escalates with advice that names a crew passing both filters.
/// A macOS-only crew with a working model leaves the frozen batch held. A
/// Linux-capable crew whose model is not refused mints that same batch.
#[test]
fn a_linux_tagged_model_refusal_resumes_for_a_crew_that_passes_both_filters() {
    const TEST: &str = "review_crew_model_access_cli::a_linux_tagged_model_refusal_resumes_for_a_crew_that_passes_both_filters";
    if !in_isolated_child(TEST) {
        return;
    }

    let (fixture, model, account) = consumer_with_pool(r#"["muse-wonder", "muse-alias"]"#);
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
    let access = (model, account);
    let runtime = open_runtime(&fixture);
    seed_access(
        &runtime,
        TASK_LOCAL_PIPELINE_JOB,
        &access,
        true,
        Duration::minutes(5),
    );
    assert_eq!(
        land_and_evaluate(&fixture, &runtime, "landed\n"),
        None,
        "no review task is minted with a refused model"
    );
    let hold = hold_of(&fixture);
    assert_eq!(hold["os_tags"], json!(["os:linux"]), "{hold}");
    assert_eq!(hold["excluded"], json!([]), "{hold}");
    assert!(
        !hold["model_refused"].as_array().unwrap().is_empty(),
        "{hold}"
    );

    let body = escalated_hold_body(&fixture, &runtime);
    let both = "whose executor can run on one of os:linux and whose model is not refused on this \
                host and account";
    assert!(
        body.contains(both),
        "[ORB-25599] the escalation requires a crew that passes both filters: {body}"
    );
    assert!(
        body.contains("a verified success of its model"),
        "[ORB-25599] a verified success of the refused model is still a way out: {body}"
    );
    assert!(
        !body.contains("admits a crew whose model is not refused."),
        "[ORB-25599] resumption must not promise a crew that ignores the OS tags: {body}"
    );
    assert!(
        !body.contains("add a crew to `workflow.purpose_crews.review` whose model is not refused"),
        "[ORB-25599] the model-only add is not enough when the task is OS-tagged: {body}"
    );
    assert!(
        !body.contains("change the definition's `os:` tags"),
        "[ORB-25599] editing os tags cannot clear a model refusal: {body}"
    );
    assert!(
        !body.contains("add a crew whose executor can run on one of"),
        "[ORB-25599] an OS-matching crew that keeps the refused model is not a way out: {body}"
    );

    let frozen = fixture.json(&["auto-task", "show", CONSUMER, "--json"]);
    let active = frozen["automation"]["state"]["active"].clone();
    assert_eq!(active["state"], "claimed", "{frozen}");
    assert!(active["action_id"].is_null(), "{frozen}");

    // The old advice is satisfied by this crew, and the OS filter still rejects it.
    pin_executor_host_os(&fixture, "claude", orbit_types::task::HostOs::Macos);
    set_policy(
        &fixture,
        POOL_KEY,
        r#"["muse-wonder", "muse-alias", "haiku"]"#,
    );
    let runtime = open_runtime(&fixture);
    let definition = runtime.auto_task_show(CONSUMER).unwrap().unwrap();
    let held = evaluate_auto_task(&runtime, &definition, false, Utc::now())
        .expect("an OS-ineligible replacement does not fail the tick");
    assert_eq!(held.reason, "stalled: crew_os_ineligible");
    let still = fixture.json(&["auto-task", "show", CONSUMER, "--json"]);
    assert_eq!(
        still["automation"]["state"]["active"]["action_key"], active["action_key"],
        "{still}"
    );
    assert_eq!(
        still["automation"]["state"]["active"]["batch"], active["batch"],
        "{still}"
    );
    assert!(
        still["automation"]["state"]["active"]["action_id"].is_null(),
        "{still}"
    );
    let mixed = &still["automation"]["state"]["stall"]["crew_hold"];
    assert!(
        mixed["excluded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|crew| crew["crew"] == "haiku"),
        "haiku is excluded by os:linux: {mixed}"
    );
    assert!(
        !mixed["model_refused"].as_array().unwrap().is_empty(),
        "the Linux reviewers stay model-refused: {mixed}"
    );

    // The crew the advice names: it can run on os:linux, and its model is not refused.
    set_policy(&fixture, "crews.luna.enabled", "true");
    set_policy(
        &fixture,
        POOL_KEY,
        r#"["muse-wonder", "muse-alias", "haiku", "luna"]"#,
    );
    let runtime = open_runtime(&fixture);
    assert_eq!(
        minted_crew(&fixture, &runtime),
        "luna",
        "the Linux-capable unrefused crew"
    );
    let resumed = fixture.json(&["auto-task", "show", CONSUMER, "--json"]);
    let state = &resumed["automation"]["state"];
    assert!(state["stall"].is_null(), "{resumed}");
    assert_eq!(state["active"]["action_key"], active["action_key"]);
    assert_eq!(state["active"]["batch"], active["batch"]);
    assert!(state["active"]["action_id"].is_string(), "{resumed}");
}
