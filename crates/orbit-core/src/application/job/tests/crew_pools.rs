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
    let failed_run_id = persisted_run(&runtime);
    let run_id = persisted_run(&runtime);
    let input = json!({ "run_id": failed_run_id, "job_run_id": run_id });

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
    assert!(
        runtime
            .read_run_state(&failed_run_id)
            .expect("read originating state")
            .expect("originating state exists")
            .activity_crew_draws
            .is_empty(),
        "ORB-13964: recovery never freezes its draw in the originating run"
    );

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

/// [ORB-14697] A pool member whose provider reads at or above its usage
/// threshold holds no ticket, and draws again once the reading's reset has
/// passed. The tickets walk every residue of the 100-ticket pool, so a draw
/// that could land on the limited member would.
#[test]
fn a_limited_pool_member_is_never_drawn_until_its_reading_resets() {
    use std::collections::BTreeMap;

    use chrono::{Duration, SubsecRound};
    use orbit_types::task::{TaskComplexity, TaskStatus};
    use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};

    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &a_limited_pool_member_is_never_drawn_until_its_reading_resets,
    )) {
        return;
    }
    let root = tempdir().expect("create tempdir");
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).expect("create global root");
    std::fs::create_dir_all(&workspace).expect("create workspace root");
    let crews = "[crews.sol]\nprovider = \"codex\"\nmodel = \"sol-model\"\n\n\
                 [crews.sonnet]\nprovider = \"claude\"\nmodel = \"sonnet-model\"\n";
    let write = |pool: &str| {
        std::fs::write(
            workspace.join("config.toml"),
            format!("[workflow]\ndefault_crew = \"sol\"\n{pool}\n{crews}"),
        )
        .expect("write crew config");
    };
    // Created before the pool exists, the task's crew is the default's, so
    // admission draws it from the whole pool.
    write("");
    let task = OrbitRuntime::from_roots(&global, &workspace)
        .expect("build runtime")
        .add_task(crate::application::task::TaskAddParams {
            title: "Provider limit draw fixture".into(),
            complexity: TaskComplexity::Medium,
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("add task");
    assert_eq!(task.crew_source.as_deref(), Some("default"));
    write("medium_complexity_crews = [\"sol:50\", \"sonnet:50\"]");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");

    let reading = |used_percent: f64, resets_at| ProviderLimitObservation {
        provider: "claude".into(),
        model: None,
        window: Some("five_hour".into()),
        exhausted: false,
        source: ProviderLimitSource::Event,
        resets_at: Some(resets_at),
        used_percent: Some(used_percent),
        window_minutes: Some(300),
        gating: true,
        observed_at: Utc::now(),
        run_id: None,
        crew: None,
        detail: String::new(),
    };
    let draws = |runtime: &OrbitRuntime| {
        let mut ticket = 16_u64;
        let mut drawn = BTreeMap::<String, usize>::new();
        let mut sources = Vec::new();
        for _ in 0..1000 {
            let mut input = json!({ "task_ids": [task.id] });
            runtime
                .install_auto_crew_admission(
                    "task_local_pipeline",
                    &mut input,
                    None,
                    false,
                    &mut || {
                        ticket += 1;
                        Ok(ticket)
                    },
                )
                .expect("admit");
            *drawn
                .entry(input["crew"].as_str().unwrap_or_default().to_string())
                .or_default() += 1;
            sources.push(input["crew_selection"]["source"].clone());
        }
        (drawn, sources)
    };

    // Whole seconds: the store keeps microseconds, but a Linux clock reads nanoseconds.
    let resets_at = (Utc::now() + Duration::hours(1)).trunc_subsecs(0);
    runtime
        .record_provider_limit(&reading(93.0, resets_at))
        .expect("seed the reading");
    let (drawn, sources) = draws(&runtime);
    assert_eq!(drawn.keys().collect::<Vec<_>>(), ["sol"], "{drawn:?}");
    let expected = format!(
        "workflow.medium_complexity_crews; provider limit: claude five_hour at 93% (limit 90%) \
         until {}; crews sonnet skipped",
        resets_at.to_rfc3339()
    );
    assert!(
        sources.iter().all(|source| source == &json!(expected)),
        "the selection names the limit: {:?}",
        sources.first()
    );

    runtime
        .record_provider_limit(&reading(93.0, Utc::now() - Duration::minutes(1)))
        .expect("record the reset reading");
    let (drawn, _) = draws(&runtime);
    assert_eq!(
        drawn.keys().collect::<Vec<_>>(),
        ["sol", "sonnet"],
        "{drawn:?}"
    );
    assert_eq!(drawn["sol"], 500, "an even split over the walked tickets");
}
