//! [ORB-25663] A follower's claimed leaf and a Muse model the follower's own
//! provider account was verified to refuse. The claimed leaf runs through the
//! real job executor with a scripted Muse CLI, so the refusal is recorded by
//! the provider completion projection exactly as on a real follower, and the
//! next claimed admission of the same automatic task reads it back.

use chrono::{Duration, Utc};
use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_core::runtime::engine::crew::BundleCrews;
use orbit_engine::RuntimeHost;
use orbit_types::resource::ExecutorResource;
use orbit_types::workflow::activity_job::Provider;
use orbit_types::workflow::{
    ExecutorDef, ExecutorSandboxKind, IMPLEMENT_ONE_STEP_ID, JobRunState,
    MUSE_MODEL_ACCESS_OUTPUT_KEY, MuseModelAccess, PipelineState, TASK_CLAIMED_LOCAL_PIPELINE_JOB,
};
use serde_json::{Value, json};

use super::provider_failure_hold::{FakeCodex, fixture_in, implementation_job};

const FOLLOWER: &str = "hm_claimed_model_access_follower";
const WONDER: &str = "muse-wonder-full-1.0";
const SPARK: &str = "muse-spark-1.3";

/// `wonder-api` is `wonder`'s model on another provider account.
const GLOBAL: &str = r#"[crews.wonder-api]
provider = "muse"
model = "muse-wonder-full-1.0"
env = ["MUSE_API_KEY=CLAIMED_MODEL_ACCESS_FIXTURE_KEY"]
"#;

const CONFIG: &str = r#"[workflow]
default_crew = "spark"
low_complexity_crews = ["wonder-api"]
medium_complexity_crews = ["wonder:1", "wonder-alias:3", "spark:1"]
hard_complexity_crews = ["wonder", "wonder-alias"]
crew_selection = "static"

[[workflow.task_types]]
name = "repair"
tags = ["repair"]

[workflow.type_complexity_crews.repair]
hard = ["wonder", "wonder-alias", "spark"]
low = ["wonder", "wonder-alias"]

[crews.wonder]
provider = "muse"
model = "muse-wonder-full-1.0"

[crews.wonder-alias]
provider = "muse"
model = "muse-wonder-full-1.0"

[crews.spark]
provider = "muse"
model = "muse-spark-1.3"
"#;

/// A claimed leaf's input as the follower's pull drain writes it: the owner's
/// snapshot of the task carries its crew, automatic source and complexity.
fn leaf(task: &str, crew: &str, source: &str, complexity: &str) -> Value {
    json!({
        "task_ids": [task],
        "claimed_task": {
            "id": task, "crew": crew, "crew_source": source, "complexity": complexity,
        },
    })
}

fn resolved(runtime: &OrbitRuntime, input: &Value) -> Result<String, OrbitError> {
    runtime
        .resolve_crew_for_run_input(input, BundleCrews::Unanimous)
        .map(|crew| crew.name)
}

fn held(runtime: &OrbitRuntime, input: &Value) -> String {
    match resolved(runtime, input) {
        Err(OrbitError::ProviderModelAccessHold(detail)) => detail,
        other => panic!("expected a provider model access hold, got {other:?}"),
    }
}

/// One claimed leaf's Muse observation, recorded as the provider completion
/// projection writes it, finished `ago` before now on `runtime`'s host.
fn seed_claimed_refusal(runtime: &OrbitRuntime, task: &str, ago: Duration) {
    let at = Utc::now() - ago;
    let input = leaf(task, "wonder", "pool:hard", "hard");
    let run = runtime
        .insert_job_run(
            TASK_CLAIMED_LOCAL_PIPELINE_JOB,
            1,
            at,
            Some(input.clone()),
            None,
        )
        .unwrap();
    let mut state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), input);
    state.pipeline = json!({
        IMPLEMENT_ONE_STEP_ID: {
            MUSE_MODEL_ACCESS_OUTPUT_KEY: serde_json::to_value(MuseModelAccess {
                model: WONDER.into(),
                crew_account: None,
                started_at: at,
                finished_at: at,
                refused: true,
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

#[test]
fn a_follower_routes_a_reclaimed_leaf_around_the_model_its_account_refused() {
    const TEST: &str = "claimed_model_access::a_follower_routes_a_reclaimed_leaf_around_the_model_its_account_refused";
    if !super::dispatch_admission::isolated(TEST) {
        return;
    }
    // Runs alone in its isolated child.
    unsafe {
        std::env::set_var("CLAIMED_MODEL_ACCESS_FIXTURE_KEY", "fixture-value");
    }
    let fx = fixture_in(Some(GLOBAL), CONFIG, Some(FOLLOWER));
    let reason = format!("model `{WONDER}` does not exist or you lack access");
    let failed = json!({
        "payload_type": "run.terminal.completed",
        "payload": {"terminal": "failed", "text": reason, "status": 403}
    });
    let completed = json!({
        "payload_type": "run.terminal.completed",
        "payload": {"terminal": "completed", "text": json!({
            "schemaVersion": 1, "status": "success", "result": {}, "error": null
        }).to_string()}
    });
    let cli = FakeCodex::named(&fx.repo, "muse", "");
    let launches = fx.repo.join("muse-launches");
    let recovered = fx.repo.join("muse-access-restored");
    std::fs::write(
        &cli.path,
        format!(
            "#!/bin/sh\nwhile [ \"$1\" != --model ]; do [ \"$#\" -gt 0 ] || exit 2; shift; done\n\
             printf '%s\\n' \"$2\" >> '{}'\n\
             if [ \"$2\" = {WONDER} ] && [ ! -e '{}' ]; then printf '%s\\n' '{failed}'; exit 1; fi\n\
             printf '%s\\n' '{completed}'\n",
            launches.display(),
            recovered.display(),
        ),
    )
    .unwrap();
    let launched = || std::fs::read_to_string(&launches).unwrap_or_default();
    let mut executor: ExecutorResource =
        serde_yaml::from_str(include_str!("../../assets/executors/muse.yaml")).unwrap();
    executor.spec.command = Some(cli.path.display().to_string());
    executor.spec.sandbox = Some(ExecutorSandboxKind::Off);
    fx.runtime
        .upsert_executor_def(&ExecutorDef::from_resource_spec(
            executor.metadata.name,
            executor.spec,
            None,
            None,
        ))
        .unwrap();
    let jobs = fx.runtime.global_root().join("resources/jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    let mut job = implementation_job(Provider::Muse);
    job.steps.remove(0);
    job.steps[0].recovery_activity = None;
    job.steps[0].resolved_recovery_activity = None;
    job.failure_activity = None;
    job.resolved_failure_activity = None;
    job.final_recovery_activity = None;
    job.resolved_final_recovery_activity = None;
    let claimed_job = jobs.join("task_claimed_local_pipeline.yaml");
    std::fs::write(
        &claimed_job,
        json!({
            "schemaVersion": 2, "kind": "Job",
            "metadata": {"name": TASK_CLAIMED_LOCAL_PIPELINE_JOB}, "spec": job,
        })
        .to_string(),
    )
    .unwrap();

    // The claimed task. Its crew comes from the owner's snapshot alone; the
    // local record only lets this fixture's agent step load its tool
    // requirements, which a real claimed envelope carries instead.
    let task = fx.tagged_task(&[], Some("spark"));
    let task_id = task.id.as_str();
    let medium = leaf(task_id, "wonder", "pool:medium", "medium");
    let hard = leaf(task_id, "wonder-alias", "pool:hard", "hard");
    let type_hard = leaf(task_id, "wonder", "type:repair/hard", "hard");
    let type_low = leaf(task_id, "wonder", "type:repair/low", "low");
    assert_eq!(resolved(&fx.runtime, &medium).unwrap(), "wonder");
    assert_eq!(resolved(&fx.runtime, &type_hard).unwrap(), "wonder");

    // The first claimed leaf runs the owner's crew and Muse refuses it.
    let first = fx
        .runtime
        .run_job_v2_from_yaml(&claimed_job, medium.clone())
        .unwrap();
    let recorded = fx.runtime.show_job_run(&first.run_id).unwrap();
    assert_eq!(recorded.state, JobRunState::Failed);
    assert_eq!(
        recorded.steps.last().unwrap().error_code.as_deref(),
        Some("muse_model_access_refused"),
    );
    assert_eq!(launched(), format!("{WONDER}\n"));
    // The claimed run persisted the evidence the follower reads back: on
    // this host, with the exact model and account.
    assert_eq!(
        recorded
            .executed_on
            .as_ref()
            .map(|at| at.machine_id.as_str()),
        Some(FOLLOWER)
    );
    let state = fx.runtime.read_run_state(&first.run_id).unwrap().unwrap();
    let access = &state.pipeline[IMPLEMENT_ONE_STEP_ID][MUSE_MODEL_ACCESS_OUTPUT_KEY];
    assert_eq!(access["model"], WONDER, "{access}");
    assert_eq!(access["refused"], true, "{access}");
    assert_eq!(access["crew_account"], Value::Null, "{access}");

    // The snapshot names the type pool even without classification inputs.
    // The hard complexity pool has no alternative, but the type pool does.
    assert_eq!(resolved(&fx.runtime, &type_hard).unwrap(), "spark");
    let mut restricted_type = type_hard.clone();
    restricted_type["allowed_crews"] = json!(["wonder"]);
    assert!(held(&fx.runtime, &restricted_type).contains(WONDER));
    // The low complexity pool has an alternative outside the named type pool.
    assert!(held(&fx.runtime, &type_low).contains(WONDER));

    // The same automatic task is claimed again on this follower: the
    // refused model leaves the draw (both of its aliases), and the leaf
    // runs the eligible alternative instead of failing a second time.
    assert_eq!(resolved(&fx.runtime, &medium).unwrap(), "spark");
    let second = fx
        .runtime
        .run_job_v2_from_yaml(&claimed_job, medium.clone())
        .unwrap();
    assert!(second.success, "{second:?}");
    assert_eq!(launched(), format!("{WONDER}\n{SPARK}\n"));

    // A pool with no eligible member left holds before any invocation.
    let detail = held(&fx.runtime, &hard);
    assert!(detail.contains(WONDER), "{detail}");
    let refused_start = fx.runtime.run_job_v2_from_yaml(&claimed_job, hard.clone());
    match refused_start {
        Err(OrbitError::ProviderModelAccessHold(_)) => {}
        other => panic!("expected the leaf to hold at crew resolution, got {other:?}"),
    }
    assert_eq!(
        launched(),
        format!("{WONDER}\n{SPARK}\n"),
        "no failed retry"
    );

    // An explicit crew — the snapshot's or the run input's — is authority.
    assert_eq!(
        resolved(&fx.runtime, &leaf(task_id, "wonder", "explicit", "hard")).unwrap(),
        "wonder"
    );
    let mut pinned = hard.clone();
    pinned["crew"] = json!("wonder");
    assert_eq!(resolved(&fx.runtime, &pinned).unwrap(), "wonder");
    // A legacy snapshot with no source keeps the owner's crew.
    let mut legacy = hard.clone();
    legacy["claimed_task"]
        .as_object_mut()
        .unwrap()
        .remove("crew_source");
    assert_eq!(resolved(&fx.runtime, &legacy).unwrap(), "wonder-alias");

    // Another host's account was never refused here.
    let other_host = OrbitRuntime::from_roots(&fx.runtime.global_root(), &fx.repo.join(".orbit"))
        .unwrap()
        .with_automation_machine_identity(Some("hm_claimed_model_access_other".into()));
    assert_eq!(resolved(&other_host, &hard).unwrap(), "wonder-alias");
    // Nor was the same model on another provider account.
    assert_eq!(
        resolved(&fx.runtime, &leaf(task_id, "wonder-api", "pool:low", "low")).unwrap(),
        "wonder-api"
    );

    // A refusal past the window has lapsed; a recent one still holds.
    seed_claimed_refusal(&fx.runtime, "ORB-90002", Duration::hours(13));
    assert_eq!(
        resolved(
            &fx.runtime,
            &leaf("ORB-90002", "wonder", "pool:hard", "hard")
        )
        .unwrap(),
        "wonder"
    );
    seed_claimed_refusal(&fx.runtime, "ORB-90003", Duration::hours(1));
    held(
        &fx.runtime,
        &leaf("ORB-90003", "wonder", "pool:hard", "hard"),
    );

    // A pinned run verifies access again; the newer success clears the
    // refusal and the automatic draw may use the model.
    std::fs::write(&recovered, "").unwrap();
    let mut pinned_medium = medium.clone();
    pinned_medium["crew"] = json!("wonder");
    let verified = fx
        .runtime
        .run_job_v2_from_yaml(&claimed_job, pinned_medium)
        .unwrap();
    assert!(verified.success, "{verified:?}");
    assert_eq!(launched(), format!("{WONDER}\n{SPARK}\n{WONDER}\n"));
    assert_eq!(resolved(&fx.runtime, &hard).unwrap(), "wonder-alias");
}
