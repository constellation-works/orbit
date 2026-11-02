//! Claims preserve admitted treatment; fresh pulls use current owner authority.
use super::claimed_review::ToOwner;

fn execute_empty_claim_worker(pair: &Pair, bound: &OrbitRuntime, run_id: &str) {
    // The real worker performs admission and persists the selection. An empty
    // job snapshot isolates that boundary from providers and delivery actions.
    let mut run = pair.follower.show_job_run_observed(run_id).unwrap();
    std::fs::create_dir_all(&pair.follower.paths().job_runs_dir).unwrap();
    std::fs::write(pair.follower.paths().job_runs_dir.join(format!("{run_id}.job.yaml")),
        format!("schemaVersion: 2\nkind: Job\nmetadata:\n  name: {}\nspec:\n  state: enabled\n  steps: []\n", run.job_id)).unwrap();
    run.state = JobRunState::Pending;
    run.started_at = None;
    run.finished_at = None;
    run.pid = None;
    run.pid_start_time = None;
    pair.follower
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&pair.follower.workspace_id().unwrap(), &run, None)
        .unwrap();
    bound.execute_pipeline_run_worker(run_id).unwrap();
}

use super::*;
use orbit_core::application::experiment::{
    ExperimentActor, ExperimentChange, ExperimentDefinitionInput,
};
use orbit_types::tool::WorkerInvocation;

fn ended_experiments_change_fresh_claims_but_not_captured_workers() {
    for end in [
        ExperimentChange::Conclude { arm: None },
        ExperimentChange::Abandon,
    ] {
        let pair = Pair::with_configs(
            "[workflow]\ndefault_crew = \"sol\"\nlow_complexity_crews = [\"sol\"]\n[experiments]\nenabled = true\n",
            "[workflow]\ndefault_crew = \"sol\"\nlow_complexity_crews = [\"sol\"]\n[experiments]\nenabled = false\n",
            &[None, None],
        );
        *pair.wire.refuse_settle.lock().unwrap() =
            Some("fixture holds settlement until admission checks finish".into());
        let owner = &pair.wire.owner;
        let actor = ExperimentActor {
            actor: "fixture operator".into(),
            reason: None,
        };
        let experiment = owner
            .create_experiment(
                "claimed-authority",
                ExperimentDefinitionInput {
                    mode: Some("live".into()),
                    primary_metric: Some("total_tokens".into()),
                    arms: Some(json!([
                        {"name":"control","control":true},
                        {"name":"treatment","overlay":{
                            "crew":{"pool":[{"crew":"luna","weight":1}]}
                        }}
                    ])),
                    ..Default::default()
                },
                actor.clone(),
            )
            .unwrap();
        owner
            .change_experiment(&experiment.id, ExperimentChange::Start, actor.clone())
            .unwrap();
        // Seed unforced enrollment to keep allocation deterministic while
        // exercising the public admission and readout boundaries below.
        use orbit_store::contracts::ExperimentStoreBackend as _;
        for task in &pair.tasks {
            owner
                .sqlite_store()
                .unwrap()
                .assign_experiments(
                    &owner.workspace_id().unwrap(),
                    task,
                    true,
                    true,
                    &|records, _, _| {
                        let record = records
                            .iter()
                            .find(|record| record.experiment.id == experiment.id)
                            .unwrap();
                        let arm = record
                            .arms
                            .iter()
                            .find(|arm| arm.name == "treatment")
                            .unwrap();
                        Ok(vec![orbit_store::contracts::ExperimentAssignment {
                            experiment: experiment.id.clone(),
                            task_id: task.clone(),
                            arm: Some(arm.name.clone()),
                            segment: Some(record.experiment.segment),
                            digest: Some(arm.overlay_digest.clone()),
                            assigned_at: Utc::now(),
                            forced: false,
                            exclusion_reason: None,
                        }])
                    },
                )
                .unwrap();
        }
        let job = pair.owner_repo.join("capture.yaml");
        std::fs::write(&job, "schemaVersion: 2\nkind: Job\nmetadata:\n  name: task_local_pipeline\nspec:\n  state: enabled\n  steps: []\n").unwrap();
        orbit_core::test_support::install_substitute_pipeline_worker([
            std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            "--list".into(),
        ]);
        for task in &pair.tasks {
            let run = owner
                .submit_job_run(
                    job.to_str().unwrap(),
                    json!({"task_id":task}),
                    Some("fixture"),
                )
                .unwrap();
            owner
                .wait_pipeline_runs(std::slice::from_ref(&run.run_id), 10, 1, Some("fixture"))
                .unwrap();
            assert_eq!(owner.get_task(task).unwrap().crew.as_deref(), Some("luna"));
        }
        let drain = pair.run_drain();
        let first = pair.running_leaf(&drain, 1);
        let admission = pair.admission(&first);
        let receipt = admission.receipt.as_ref().unwrap();
        let capture = receipt.experiments.clone().unwrap();
        assert!(capture.assignments[0].live);
        let claim = receipt.claim.as_ref().unwrap();
        let bound = pair
            .follower
            .clone()
            .with_worker_invocation(
                WorkerInvocation {
                    owner_machine_id: OWNER.into(),
                    owner_workspace_id: admission.destination.owner_workspace_id.clone(),
                    owner_destination: admission.destination.selector.clone(),
                    task_id: claim.task_id.clone(),
                    claim_id: claim.claim_id.clone(),
                    execution: claim.executed_on.clone(),
                    bound_run_id: first.clone(),
                },
                Arc::new(ToOwner(owner.clone())),
            )
            .unwrap();
        owner.change_experiment(&experiment.id, end, actor).unwrap();
        let second = pair.running_leaf(&drain, 2);
        let second_admission = pair.admission(&second);
        let receipt = second_admission.receipt.as_ref().unwrap();
        assert!(!receipt.experiments.as_ref().unwrap().assignments[0].live);
        assert_eq!(
            receipt.task.as_ref().unwrap().crew.as_deref(),
            Some("sol"),
            "fresh claim eligibility and admitted crew use normal routing"
        );
        let second_claim = receipt.claim.as_ref().unwrap();
        let second_bound = pair
            .follower
            .clone()
            .with_worker_invocation(
                WorkerInvocation {
                    owner_machine_id: OWNER.into(),
                    owner_workspace_id: second_admission.destination.owner_workspace_id.clone(),
                    owner_destination: second_admission.destination.selector.clone(),
                    task_id: second_claim.task_id.clone(),
                    claim_id: second_claim.claim_id.clone(),
                    execution: second_claim.executed_on.clone(),
                    bound_run_id: second.clone(),
                },
                Arc::new(ToOwner(owner.clone())),
            )
            .unwrap();
        execute_empty_claim_worker(&pair, &bound, &first);
        let selection = bound
            .read_run_state(&first)
            .unwrap()
            .unwrap()
            .crew_selection
            .unwrap();
        assert_eq!(selection["crew"], "luna");
        assert_eq!(
            selection["source"],
            format!("experiment:{}:treatment", experiment.id)
        );
        // A restarted worker uses its persisted selection after conclusion.
        execute_empty_claim_worker(&pair, &bound, &first);
        assert_eq!(
            bound
                .read_run_state(&first)
                .unwrap()
                .unwrap()
                .crew_selection,
            Some(selection)
        );
        assert_eq!(
            pair.admission(&first).receipt.unwrap().experiments,
            Some(capture)
        );
        execute_empty_claim_worker(&pair, &second_bound, &second);
        assert_eq!(
            RuntimeHost::agent_crew_config_for_input(
                &second_bound,
                &json!({"run_id":second,"task_id":second_claim.task_id})
            )
            .unwrap()
            .unwrap()
            .name
            .as_deref(),
            Some("sol")
        );
        // Running claims fence ordinary task edits; an operator pin belongs
        // to a later fresh admission, rather than mutating this contract.
        let pin = owner.update_task_as_human(
            &claim.task_id,
            orbit_core::application::task::TaskUpdateParams {
                crew: Some(Some("sol".into())),
                ..Default::default()
            },
            "fixture operator".into(),
        );
        assert!(matches!(pin, Err(OrbitError::InvalidInput(_))));
        owner
            .serve_claim_settle(
                &ToolSessionContext {
                    caller_machine_id: Some(FOLLOWER.into()),
                    process_machine_id: Some(OWNER.into()),
                    transport: Some(McpTransport::SshMcp),
                    effective_capabilities: BTreeSet::from([McpCapability::Agent]),
                    ..Default::default()
                },
                &claim.claim_id,
                Some(&first),
                ClaimMutation::Fail(ClaimEvidence {
                    summary: Some("fixture completes admission checks".into()),
                    crew: Some("luna".into()),
                    ..Default::default()
                }),
                orbit_store::contracts::ClaimAttribution::default(),
            )
            .unwrap();
        owner
            .update_task_as_human(
                &claim.task_id,
                orbit_core::application::task::TaskUpdateParams {
                    crew: Some(Some("sol".into())),
                    ..Default::default()
                },
                "fixture operator".into(),
            )
            .unwrap();
        let pinned = owner.get_task(&claim.task_id).unwrap();
        assert_eq!(pinned.crew.as_deref(), Some("sol"));
        assert_eq!(pinned.crew_source.as_deref(), Some("explicit"));
        let readout = owner.experiment_readout(&experiment.id, None).unwrap();
        let treatment = readout
            .arms
            .iter()
            .find(|arm| arm.name == "treatment")
            .unwrap();
        assert_eq!(treatment.n, 2);
        assert_eq!(
            treatment.delivery.as_ref().unwrap().delivered,
            1,
            "the fresh suppressed claim cannot borrow its old owner's treatment; the settled original claim retains its actual treatment"
        );
        assert!(
            pair.follower
                .task_experiments(&claim.task_id)
                .unwrap()
                .is_empty()
        );
    }
}
crate::scenario_table!(
    experiment_admission_scenarios,
    ended_experiments_change_fresh_claims_but_not_captured_workers
);
