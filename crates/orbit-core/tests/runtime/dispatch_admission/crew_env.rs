use super::*;

const SOURCE: &str = "FIXTURE_CREW_READINESS_UNSET_SOURCE";

#[test]
fn crew_env_waits_and_sustained_pool_exclusion_are_visible() {
    if !isolated(
        "dispatch_admission::crew_env::crew_env_waits_and_sustained_pool_exclusion_are_visible",
    ) {
        return;
    }
    let config = format!(
        r#"
[workflow]
default_crew = "sol"
medium_complexity_crews = ["sol", "credits"]
[crews.sol]
provider = "codex"
model = "fixture-shared"
[crews.credits]
provider = "codex"
model = "fixture-credits"
env = ["OPENAI_API_KEY={SOURCE}"]
"#
    );
    let (_root, mut runtime, _) = configured_runtime(&config, &["src/fixture.rs"]);
    let task = runtime
        .add_task(TaskAddParams {
            title: "Crew env readiness fixture".into(),
            description: "Reproduce the unset-source admission incident".into(),
            acceptance_criteria: vec!["Wait names the missing source".into()],
            plan: "Observe admission".into(),
            complexity: TaskComplexity::Medium,
            context_files: vec!["file:src/fixture.rs".into()],
            crew: Some("credits".into()),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap();
    let backlog = list_backlog_tasks(&runtime, json!({"allowed_crews": ["sol"]}));
    let excluded = backlog["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == task.id)
        .unwrap();
    assert_eq!(excluded["reason"], "crew_env_unset", "{backlog}");
    assert_eq!(excluded["crew"], "credits");
    assert!(excluded["detail"].as_str().unwrap().contains(SOURCE));
    let readiness = default_readiness(&runtime);
    let entry = readiness_task(&readiness, &task.id.to_string());
    assert_eq!(entry["reason"], "crew_env_unset", "{entry}");
    assert!(entry["detail"].as_str().unwrap().contains(SOURCE));

    let drain = running_run(
        &runtime,
        "workspace_auto_pipeline",
        json!({"auto_crew_pools": runtime.workspace_complexity_crew_pools().unwrap()}),
    );
    classify(&runtime, &drain);
    let mut state = runtime.read_run_state(&drain).unwrap().unwrap();
    assert_eq!(state.pull_drain.crew_pool_exclusions["credits"].passes, 1);
    let waiting = state
        .pull_drain
        .drain_last_pass
        .as_ref()
        .unwrap()
        .excluded
        .iter()
        .find(|entry| entry.task_id == task.id)
        .unwrap();
    assert_eq!(waiting.reason.as_deref(), Some("crew_env_unset"));
    assert!(waiting.detail.as_deref().unwrap().contains(SOURCE));
    assert!(state.pull_drain.pool_crew_exclusion_warnings().is_empty());
    // Backdate only the first observation, without sleeping or generating load.
    state
        .pull_drain
        .crew_pool_exclusions
        .get_mut("credits")
        .unwrap()
        .since -= chrono::Duration::seconds(121);
    runtime.write_run_state(&drain, &state).unwrap();
    classify(&runtime, &drain);
    let state = runtime.read_run_state(&drain).unwrap().unwrap();
    assert_eq!(state.pull_drain.crew_pool_exclusions["credits"].passes, 2);
    let readiness = default_readiness(&runtime);
    assert!(
        readiness["crew_pool_warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains(SOURCE)),
        "{readiness}"
    );
    let run = runtime.show_job_run_observed(&drain).unwrap();
    let shown = orbit_core::application::job::job_run_to_json(&run, Some(&state));
    assert_eq!(shown["crew_pool_warnings"], readiness["crew_pool_warnings"]);
    let frictions = orbit_store::friction_store::FrictionStore::open(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
        runtime.data_root().join("frictions"),
    )
    .unwrap();
    let filed = frictions
        .list(&orbit_store::contracts::FrictionListFilter::default())
        .unwrap();
    assert_eq!(filed.len(), 1, "one sustained incident");
    classify(&runtime, &drain);
    assert_eq!(
        frictions
            .list(&orbit_store::contracts::FrictionListFilter::default())
            .unwrap()
            .len(),
        1,
        "continued passes deduplicate the same incident"
    );
    // The launching environment is refreshed deterministically through the
    // same defaults seam clock ticks use; no process-global env mutation.
    runtime
        .context
        .apply_child_env_defaults(&[(SOURCE.into(), "fixture-secret-never-print".into())]);
    classify(&runtime, &drain);
    let state = runtime.read_run_state(&drain).unwrap().unwrap();
    assert!(
        state.pull_drain.crew_pool_exclusions.is_empty(),
        "recovery resets the consecutive streak"
    );
    let readiness = default_readiness(&runtime);
    assert!(
        readiness["crew_pool_warnings"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!readiness.to_string().contains("fixture-secret-never-print"));
}

#[test]
fn readiness_preserves_the_live_drains_unset_source_in_a_different_environment() {
    if !isolated(
        "dispatch_admission::crew_env::readiness_preserves_the_live_drains_unset_source_in_a_different_environment",
    ) {
        return;
    }
    let config = format!(
        r#"
[workflow]
default_crew = "sol"
[crews.sol]
provider = "codex"
model = "fixture-shared"
[crews.credits]
provider = "codex"
model = "fixture-credits"
env = ["OPENAI_API_KEY={SOURCE}"]
"#
    );
    let (_root, mut runtime, _) = configured_runtime(&config, &["src/fixture.rs"]);
    let task = runtime
        .add_task(TaskAddParams {
            title: "Different readiness launcher".into(),
            description: "Observer holds a source the live drain lacks".into(),
            acceptance_criteria: vec!["Readiness names the drain's cause".into()],
            plan: "Observe the existing drain".into(),
            complexity: TaskComplexity::Medium,
            context_files: vec!["file:src/fixture.rs".into()],
            crew: Some("credits".into()),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap();
    let drain = running_run(
        &runtime,
        "workspace_auto_pipeline",
        json!({"allowed_crews": ["sol"]}),
    );
    // A missing observer source must not override the run's deliberate
    // restriction when that run recorded no missing sources.
    let readiness = default_readiness(&runtime);
    let entry = readiness_task(&readiness, &task.id.to_string());
    assert_eq!(entry["reason"], "crew_not_allowed", "{entry}");
    runtime
        .context
        .apply_child_env_defaults(&[(SOURCE.into(), "observer-secret-never-print".into())]);
    let mut state = runtime.read_run_state(&drain).unwrap().unwrap();
    state
        .crew_env_unset
        .push(orbit_types::workflow::CrewEnvUnset {
            crew: "credits".into(),
            env: vec![
                orbit_types::identity::CrewEnvVar::parse(&format!("OPENAI_API_KEY={SOURCE}"))
                    .unwrap(),
            ],
        });
    runtime.write_run_state(&drain, &state).unwrap();
    let readiness = default_readiness(&runtime);
    let entry = readiness_task(&readiness, &task.id.to_string());
    assert_eq!(entry["reason"], "crew_env_unset", "{entry}");
    assert!(entry["detail"].as_str().unwrap().contains(SOURCE));
    assert!(
        !readiness
            .to_string()
            .contains("observer-secret-never-print")
    );
}
