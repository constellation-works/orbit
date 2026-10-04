//! The `workflow.final_recovery_crews` draw: once per run, frozen in run
//! state, and never outside the run's crew allowlist.

use std::cell::Cell;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::workflow::{FINAL_RECOVERY_CREWS_KEY, PipelineState};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

use crate::OrbitRuntime;

/// Sorted canonically, the pool below is `opus:20, sol:100`: tickets 0..20
/// draw opus and 20..120 draw sol. Ticket values at or above 16 clear the
/// rejection-sampling threshold for a 120-ticket pool.
const OPUS_TICKET: u64 = 120;
const SOL_TICKET: u64 = 120 + 50;

fn runtime_with_pool(pool: &str) -> (TempDir, OrbitRuntime) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    std::fs::write(
        workspace_root.join("config.toml"),
        format!(
            r#"[workflow]
default_crew = "sol"
final_recovery_crews = {pool}

[crews.sol]
provider = "codex"
model = "sol-model"

[crews.opus]
provider = "claude"
model = "opus-model"

[crews.grok]
provider = "grok"
model = "grok-model"
"#
        ),
    )
    .expect("write crew config");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime)
}

/// A run seeded the way execution seeds one before its first step.
fn persisted_run(runtime: &OrbitRuntime) -> String {
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), Some(json!({})), None)
        .expect("insert run");
    runtime
        .stores()
        .jobs()
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({})),
        )
        .expect("seed run state");
    run.run_id
}

fn draw(runtime: &OrbitRuntime, input: &Value, ticket: u64) -> Result<String, OrbitError> {
    runtime
        .final_recovery_crew(input, &mut || Ok(ticket))
        .map(|crew| crew.name)
}

#[test]
fn final_recovery_crew_is_drawn_once_and_frozen_for_the_run() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &final_recovery_crew_is_drawn_once_and_frozen_for_the_run,
    )) {
        return;
    }
    let (_root, runtime) = runtime_with_pool(r#"["sol:100", "opus:20"]"#);
    let run_id = persisted_run(&runtime);
    let input = json!({ "run_id": run_id });

    assert_eq!(
        draw(&runtime, &input, OPUS_TICKET).expect("first draw"),
        "opus"
    );
    let calls = Cell::new(0);
    let again = runtime
        .final_recovery_crew(&input, &mut || {
            calls.set(calls.get() + 1);
            Ok(SOL_TICKET)
        })
        .expect("frozen draw");
    assert_eq!(
        again.name, "opus",
        "a later dispatch reuses the frozen crew"
    );
    assert_eq!(calls.get(), 0, "a frozen draw never rolls again");

    let state = runtime
        .read_run_state(&run_id)
        .expect("read state")
        .expect("state exists");
    let frozen = &state.activity_crew_draws[FINAL_RECOVERY_CREWS_KEY];
    assert_eq!(frozen.crew, "opus");
    assert_eq!(
        frozen
            .eligible_pool
            .iter()
            .map(|member| (member.name.as_str(), member.weight))
            .collect::<Vec<_>>(),
        [("opus", 20), ("sol", 100)]
    );

    let other_run = persisted_run(&runtime);
    assert_eq!(
        draw(&runtime, &json!({ "run_id": other_run }), SOL_TICKET).expect("independent run"),
        "sol",
        "each run draws for itself"
    );
}

#[test]
fn final_recovery_crew_draw_honours_the_run_allowlist() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &final_recovery_crew_draw_honours_the_run_allowlist,
    )) {
        return;
    }
    let (_root, runtime) = runtime_with_pool(r#"["sol:100", "opus:20"]"#);
    let input = json!({ "run_id": persisted_run(&runtime), "allowed_crews": ["sol"] });
    assert_eq!(
        draw(&runtime, &input, OPUS_TICKET).expect("allowlisted draw"),
        "sol",
        "a ticket that would land on an excluded crew draws over the permitted members"
    );

    let excluded = json!({ "run_id": persisted_run(&runtime), "allowed_crews": ["grok"] });
    let error = draw(&runtime, &excluded, SOL_TICKET).expect_err("no member permitted");
    assert!(
        error.to_string().contains("allowlist"),
        "the refusal names the allowlist: {error}"
    );
}

#[test]
fn empty_final_recovery_pool_refuses_to_draw() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &empty_final_recovery_pool_refuses_to_draw,
    )) {
        return;
    }
    let (_root, runtime) = runtime_with_pool("[]");
    let error = draw(
        &runtime,
        &json!({ "run_id": persisted_run(&runtime) }),
        SOL_TICKET,
    )
    .expect_err("[] disables final recovery");
    assert!(error.to_string().contains("disabled"), "{error}");
}
