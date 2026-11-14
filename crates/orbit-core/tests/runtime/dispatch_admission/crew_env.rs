use super::*;

const SOURCE: &str = "FIXTURE_CREW_READINESS_UNSET_SOURCE";

#[test]
fn crew_env_waits_and_sustained_pool_exclusion_are_visible() {
    if !isolated(
        "dispatch_admission::crew_env::crew_env_waits_and_sustained_pool_exclusion_are_visible",
    ) {
        return;
    }
    for crew in ["credits", "credits@high"] {
        check_crew_env_waits_and_pool_health(crew);
    }
}

fn check_crew_env_waits_and_pool_health(crew: &str) {
    let config = format!(
        r#"
[workflow]
default_crew = "sol"
medium_complexity_crews = ["sol", "{crew}"]
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
            crew: Some(crew.into()),
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
    assert_eq!(state.pull_drain.crew_pool_exclusions[crew].passes, 1);
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
        .get_mut(crew)
        .unwrap()
        .since -= chrono::Duration::seconds(121);
    runtime.write_run_state(&drain, &state).unwrap();
    classify(&runtime, &drain);
    let state = runtime.read_run_state(&drain).unwrap().unwrap();
    assert_eq!(state.pull_drain.crew_pool_exclusions[crew].passes, 2);
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
    assert_eq!(readiness_task(&readiness, &task.id)["reason"], "ready");
    assert!(admitted(&list_backlog_tasks(&runtime, json!({}))).contains(&task.id));
    assert_eq!(
        runtime.get_task(&task.id).unwrap().crew.as_deref(),
        Some(crew)
    );
    let restricted = running_run(
        &runtime,
        "workspace_auto_pipeline",
        json!({
            "allowed_crews": ["sol"],
            "auto_crew_pools": runtime.workspace_complexity_crew_pools().unwrap(),
        }),
    );
    classify(&runtime, &restricted);
    let state = runtime.read_run_state(&restricted).unwrap().unwrap();
    assert!(
        state.pull_drain.crew_pool_exclusions[crew]
            .reason
            .contains("outside this drain's allowed window")
    );
}

#[test]
fn two_pool_members_of_one_family_report_the_unset_crew_once() {
    if !isolated(
        "dispatch_admission::crew_env::two_pool_members_of_one_family_report_the_unset_crew_once",
    ) {
        return;
    }
    let _env = orbit_common::test_env::unset([SOURCE]);
    let crews = format!(
        r#"
[crews.sol]
provider = "codex"
model = "fixture-shared"
[crews.opus]
provider = "claude"
model = "claude-opus"
env = ["OPENAI_API_KEY={SOURCE}"]
"#
    );
    let config = format!(
        r#"
[workflow]
default_crew = "sol"
hard_complexity_crews = ["sol"]
{crews}"#
    );
    let (_root, runtime, _) = configured_runtime(&config, &["src/fixture.rs"]);
    let task = runtime
        .add_task(TaskAddParams {
            title: "Same-family unset crew fixture".into(),
            description: "Two pool members share one unset crew family".into(),
            acceptance_criteria: vec!["The unset crew is named once".into()],
            plan: "Observe admission".into(),
            complexity: TaskComplexity::Hard,
            context_files: vec!["file:src/fixture.rs".into()],
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap();
    // Creation drew `sol`, the hard pool's only member then. Replace the
    // pool with two references to one unset family and reopen, so admission
    // draws the whole new pool, as it does for an unassigned hard task.
    let config = format!(
        r#"
[workflow]
default_crew = "sol"
hard_complexity_crews = ["opus@medium:100", "opus:100"]
{crews}"#
    );
    std::fs::write(runtime.paths().global_dir.join("config.toml"), config).unwrap();
    let runtime = OrbitRuntime::from_roots(&runtime.paths().global_dir, &runtime.paths().orbit_dir)
        .unwrap()
        .with_host_resource_probe(PressureProbe::calm());
    let backlog = list_backlog_tasks(&runtime, json!({}));
    let excluded = backlog["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == task.id)
        .unwrap();
    assert_eq!(excluded["reason"], "crew_env_unset", "{backlog}");
    assert_eq!(excluded["crew"], "opus", "{backlog}");
    let detail = excluded["detail"].as_str().unwrap();
    assert_eq!(detail.matches(SOURCE).count(), 1, "{detail}");
    assert_eq!(detail.matches("crew `opus`").count(), 1, "{detail}");
    let readiness = default_readiness(&runtime);
    let entry = readiness_task(&readiness, &task.id.to_string());
    assert_eq!(entry["reason"], "crew_env_unset", "{entry}");
    assert_eq!(entry["crew"], "opus", "{entry}");
    assert_eq!(
        entry["detail"].as_str().unwrap().matches(SOURCE).count(),
        1,
        "{entry}"
    );
}

#[test]
fn readiness_preserves_the_live_drains_unset_source_in_a_different_environment() {
    if !isolated(
        "dispatch_admission::crew_env::readiness_preserves_the_live_drains_unset_source_in_a_different_environment",
    ) {
        return;
    }
    for crew in ["credits", "credits@high"] {
        check_live_drain_unset_source_readiness(crew);
    }
}

fn check_live_drain_unset_source_readiness(crew: &str) {
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
            crew: Some(crew.into()),
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
            file_problems: Default::default(),
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

fn reopen_submission_runtime(runtime: &OrbitRuntime) -> OrbitRuntime {
    OrbitRuntime::from_roots_with_binding(
        &runtime.paths().global_dir,
        &runtime.paths().orbit_dir,
        runtime.workspace_runtime_binding().unwrap().clone(),
    )
    .unwrap()
    .with_host_resource_probe(PressureProbe::calm())
}

/// The submission probe reaches a real owner; no provider worker is launched.
struct CredentialOwnerProbe {
    owner: OrbitRuntime,
    calls: AtomicUsize,
}

impl orbit_tools::DrainOwnerTransport for CredentialOwnerProbe {
    fn call(&self, _selector: &str, name: &str, input: Value) -> Result<Value, OrbitError> {
        assert_eq!(name, "orbit.drain.probe");
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.owner.run_tool_with_context(
            name,
            input,
            ToolContext {
                session_context: ToolSessionContext {
                    caller_machine_id: Some("credential-follower".into()),
                    process_machine_id: Some("credential-owner".into()),
                    transport: Some(McpTransport::SshMcp),
                    effective_capabilities: BTreeSet::from([McpCapability::Agent]),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
    }

    fn show_task(&self, _selector: &str, _input: Value) -> Result<Value, OrbitError> {
        panic!("submission must not read an owner task")
    }

    fn worker_coordinator(&self) -> Arc<dyn orbit_tools::OwnerCoordinator> {
        panic!("the substitute worker never starts")
    }
}

#[test]
fn owner_and_pull_windows_exclude_unset_crew_families_and_preserve_available_effort() {
    if !isolated(
        "dispatch_admission::crew_env::owner_and_pull_windows_exclude_unset_crew_families_and_preserve_available_effort",
    ) {
        return;
    }
    use orbit_core::application::job::JobRunListParams;

    let _env = orbit_common::test_env::unset([SOURCE]);
    // env is global-only: install it in the fixture's global layer after open.
    let config = format!(
        r#"[workflow]
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
    for crew in ["credits", "credits@high"] {
        let (_root, owner) = super::super::review_run_option::open(ShipMode::Pr, "", true);
        std::fs::write(owner.paths().global_dir.join("config.toml"), &config).unwrap();
        let mut owner = reopen_submission_runtime(&owner);
        owner = owner.with_automation_machine_identity(Some("credential-owner".into()));
        let (_follower_root, follower) =
            super::super::review_run_option::open(ShipMode::Pr, "", true);
        std::fs::write(follower.paths().global_dir.join("config.toml"), &config).unwrap();
        let probe = Arc::new(CredentialOwnerProbe {
            owner: owner.clone(),
            calls: AtomicUsize::new(0),
        });
        let mut follower = reopen_submission_runtime(&follower)
            .with_automation_machine_identity(Some("credential-follower".into()))
            .with_coordination_write_owner(Some("credential-owner".into()))
            .with_drain_owner_transport(probe.clone());
        let submit = |runtime: &OrbitRuntime, names: &[String], pull: bool| {
            if pull {
                runtime.submit_workspace_pull_run(
                    orbit_core::WorkspacePullRequest {
                        selector: "credential-owner/ws_review_option",
                        for_seconds: Some(60),
                        allowed_crews: names,
                        ..Default::default()
                    },
                    JobRunTrigger::cli(),
                )
            } else {
                runtime.submit_workspace_auto_run_with_mode(
                    Some(60),
                    Some(1),
                    CompletionPolicy::Review,
                    names,
                    &Default::default(),
                    None,
                    None,
                    JobRunTrigger::cli(),
                    false,
                    false,
                    false,
                    None,
                    None,
                )
            }
        };
        for (runtime, pull) in [(&owner, false), (&follower, true)] {
            let error = submit(runtime, &[crew.into()], pull).unwrap_err();
            assert!(matches!(error, OrbitError::InvalidInput(_)), "{error}");
            assert!(error.to_string().contains(SOURCE), "{error}");
            assert!(
                runtime
                    .list_job_runs(JobRunListParams::default())
                    .unwrap()
                    .is_empty()
            );
        }
        assert_eq!(
            probe.calls.load(Ordering::SeqCst),
            0,
            "blocked-only pull never probes"
        );

        for available in [false, true] {
            if available {
                for runtime in [&mut owner, &mut follower] {
                    runtime.context.apply_child_env_defaults(&[(
                        SOURCE.into(),
                        "submission-secret-never-print".into(),
                    )]);
                }
            }
            for (runtime, pull) in [(&owner, false), (&follower, true)] {
                let names = [crew.into(), "sol@high".into()];
                match submit(runtime, &names, pull) {
                    Ok(_) => {}
                    Err(OrbitError::Execution(message))
                        if message.contains("spawn pipeline worker") => {}
                    other => panic!("expected admission without launching a worker: {other:?}"),
                }
                let runs = runtime
                    .list_job_runs(JobRunListParams {
                        job_id: Some(
                            if pull {
                                "workspace_pull_pipeline"
                            } else {
                                "workspace_auto_pipeline"
                            }
                            .into(),
                        ),
                        limit: Some(1),
                        ..Default::default()
                    })
                    .unwrap();
                let input = runs[0].input.as_ref().unwrap();
                let state = runtime.read_run_state(&runs[0].run_id).unwrap().unwrap();
                let captured = state
                    .crew_env_unset
                    .iter()
                    .find(|unset| unset.crew == "credits");
                assert_eq!(captured.is_none(), available);
                if let Some(captured) = captured {
                    assert!(captured.describe().contains(SOURCE));
                }
                assert!(
                    !serde_json::to_string(&state)
                        .unwrap()
                        .contains("submission-secret-never-print")
                );
                assert_eq!(
                    input["allowed_crews"],
                    if available {
                        json!([crew, "sol@high"])
                    } else {
                        json!(["sol@high"])
                    }
                );
                assert!(!input.to_string().contains("submission-secret-never-print"));
            }
        }
        assert!(
            probe.calls.load(Ordering::SeqCst) > 0,
            "mixed and recovered pull windows reach the owner"
        );
    }
}
