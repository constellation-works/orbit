//! Claims preserve admitted treatment; fresh pulls use current owner authority.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::claimed_review::ToOwner;
use orbit_core::application::crew_memory::{MemoryFile, MemoryWrite};
use orbit_engine::{V2AuditWriter, V2DispatchInput, dispatch_v2_activity};
use orbit_types::workflow::activity_job::{ActivityV2Spec, AgentLoopSpec, Provider};

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
const OWNER_NOTE: &str = "Keep the change local and the note short.\n";
const SOL_MODEL: &str = orbit_common::model_defaults::CODEX_SOL_MODEL;

/// Drops only the crew-memory policy read. Every other owner call is served,
/// so a missing observation is the only difference from a normal binding.
struct DropMemoryPolicy(OrbitRuntime);

impl OwnerCoordinator for DropMemoryPolicy {
    fn call(
        &self,
        name: &str,
        input: Value,
        session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        if input.get("_worker_read").and_then(Value::as_str) == Some("crew_memory_policy") {
            return Err(OrbitError::PolicyDenied(
                "fixture withholds the owner crew memory policy".into(),
            ));
        }
        ToOwner(self.0.clone()).call(name, input, session)
    }
}

fn write_note(runtime: &OrbitRuntime, text: &str) {
    runtime
        .crew_memory_write(
            SOL_MODEL,
            MemoryWrite {
                files: vec![MemoryFile {
                    path: "notes.md".into(),
                    text: text.into(),
                }],
                evidence: Vec::new(),
            },
            Some("fixture note".into()),
        )
        .unwrap();
}

fn set_crew_memory_enabled(runtime: &OrbitRuntime, enabled: bool) {
    let path = runtime.shared_root().join("config.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    let value = if enabled { "true" } else { "false" };
    let mut updated = None;
    for current in ["true", "false"] {
        let needle = format!("[crew_memory]\nenabled = {current}");
        if text.contains(&needle) {
            updated = Some(text.replace(&needle, &format!("[crew_memory]\nenabled = {value}")));
            break;
        }
    }
    let updated = updated.expect("workspace config names crew_memory.enabled");
    assert_ne!(
        updated,
        text,
        "crew memory key at {} did not change",
        path.display()
    );
    std::fs::write(&path, updated).unwrap();
    assert_eq!(runtime.current_crew_memory_enabled(), enabled);
}

fn bind_leaf(pair: &Pair, leaf: &str, coordinator: Arc<dyn OwnerCoordinator>) -> OrbitRuntime {
    let admission = pair.admission(leaf);
    let receipt = admission.receipt.as_ref().unwrap();
    let claim = receipt.claim.as_ref().unwrap();
    pair.follower
        .clone()
        .with_worker_invocation(
            WorkerInvocation {
                owner_machine_id: OWNER.into(),
                owner_workspace_id: admission.destination.owner_workspace_id.clone(),
                owner_destination: admission.destination.selector.clone(),
                task_id: claim.task_id.clone(),
                claim_id: claim.claim_id.clone(),
                execution: claim.executed_on.clone(),
                bound_run_id: leaf.to_string(),
            },
            coordinator,
        )
        .unwrap()
}

fn install_fake_codex(dir: &Path) -> (PathBuf, PathBuf) {
    let program = dir.join("codex-fake");
    let stdin_path = dir.join("stdin.txt");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\ncat > '{}'\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{\"ok\":true}},\"error\":null}}'\n",
            stdin_path.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (program, stdin_path)
}

fn point_codex_at(runtime: &OrbitRuntime, program: &Path) {
    if let Some(mut def) = runtime.get_executor_def("codex").unwrap() {
        def.command = Some(program.display().to_string());
        def.sandbox = None;
        runtime.upsert_executor_def(&def).unwrap();
    }
}

fn dispatch_envelope(host: &OrbitRuntime, run_id: &str, task_id: &str, stdin_path: &Path) -> Value {
    let audit_dir = tempfile::tempdir().unwrap();
    let audit = V2AuditWriter::with_disk_sinks(
        audit_dir.path(),
        Arc::new(orbit_store::Store::open_in_memory().unwrap()),
        "ws_memory",
        run_id,
        "codex:test".to_string(),
        None,
    )
    .unwrap();
    let spec = ActivityV2Spec::AgentLoop(AgentLoopSpec {
        model: Some(SOL_MODEL.to_string()),
        require_response_envelope: true,
        ..AgentLoopSpec::for_provider(
            "Return the requested response envelope.",
            Provider::Codex,
            30,
        )
    });
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: "agent_implement",
        spec: &spec,
        fs_profile: None,
        input: json!({"task_id": task_id, "prompt": "Implement the task."}),
        audit,
        run_id,
        host: Some(host),
    })
    .unwrap_or_else(|error| panic!("dispatch {run_id} failed: {error}"));
    assert!(
        outcome.success,
        "dispatch {run_id} did not succeed: {:?}",
        outcome.message
    );
    let stdin = std::fs::read_to_string(stdin_path).unwrap();
    let marker = "\nExecution envelope:\n";
    let json = stdin
        .rsplit_once(marker)
        .unwrap_or_else(|| panic!("provider stdin has no execution envelope:\n{stdin}"))
        .1
        .trim();
    serde_json::from_str(json).unwrap_or_else(|error| panic!("envelope JSON: {error}: {json}"))
}

fn memory_note(value: &Value) -> Option<&str> {
    value
        .pointer("/deliveries/0/block/model/0/text")
        .and_then(Value::as_str)
}

fn assert_captured_owner_note(memory: &Value) {
    assert_eq!(memory["enabled"], true, "{memory}");
    assert_eq!(
        memory
            .pointer("/deliveries/0/model")
            .and_then(Value::as_str),
        Some(SOL_MODEL),
        "{memory}"
    );
    assert_eq!(memory_note(memory), Some(OWNER_NOTE), "{memory}");
}

/// An enabled capture stays on the receipt after the owner turns crew memory
/// off. The next claimed dispatch, and an owner dispatch of the same capture,
/// omit `crew_memory`. While the owner key stays on, the leaf still sends the
/// captured owner bytes even though its own key is off. A follower checkout
/// cannot write memory, so those bytes cannot have been read from its tables.
/// A missing owner observation delivers nothing.
fn owner_disable_suppresses_claimed_memory_delivery() {
    let pair = Pair::with_configs(
        "[workflow]\ndefault_crew = \"sol\"\nlow_complexity_crews = [\"sol\"]\n[experiments]\nenabled = true\n[crew_memory]\nenabled = true\n",
        "[workflow]\ndefault_crew = \"sol\"\nlow_complexity_crews = [\"sol\"]\n[experiments]\nenabled = false\n[crew_memory]\nenabled = true\n",
        &[None],
    );
    *pair.wire.refuse_settle.lock().unwrap() =
        Some("fixture holds settlement until the memory envelopes are checked".into());
    let owner = &pair.wire.owner;
    write_note(owner, OWNER_NOTE);
    let actor = ExperimentActor {
        actor: "fixture operator".into(),
        reason: None,
    };
    let experiment = owner
        .create_experiment(
            "claimed-memory",
            ExperimentDefinitionInput {
                mode: Some("live".into()),
                primary_metric: Some("total_tokens".into()),
                arms: Some(json!([
                    {"name":"control","control":true},
                    {"name":"treatment","overlay":{"memory":{"roles":["implement"]}}}
                ])),
                ..Default::default()
            },
            actor.clone(),
        )
        .unwrap();
    owner
        .change_experiment(&experiment.id, ExperimentChange::Start, actor)
        .unwrap();
    use orbit_store::contracts::ExperimentStoreBackend as _;
    let task = &pair.tasks[0];
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
    let job = pair.owner_repo.join("capture.yaml");
    std::fs::write(
        &job,
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: task_local_pipeline\nspec:\n  state: enabled\n  steps: []\n",
    )
    .unwrap();
    orbit_core::test_support::install_substitute_pipeline_worker([
        std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        "--list".into(),
    ]);
    let owner_run = owner
        .submit_job_run(
            job.to_str().unwrap(),
            json!({"task_id": task}),
            Some("fixture"),
        )
        .unwrap();
    owner
        .wait_pipeline_runs(
            std::slice::from_ref(&owner_run.run_id),
            10,
            1,
            Some("fixture"),
        )
        .unwrap();
    let owner_capture = owner
        .show_job_run(&owner_run.run_id)
        .unwrap()
        .input
        .unwrap()["experiment_capture"]
        .clone();
    let owner_memory = owner_capture["assignments"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|entry| entry.get("memory").filter(|memory| !memory.is_null()))
        .unwrap()
        .clone();
    assert_captured_owner_note(&owner_memory);

    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let receipt_experiments = pair.admission(&leaf).receipt.unwrap().experiments.unwrap();
    let claimed_memory = receipt_experiments
        .assignments
        .iter()
        .find_map(|entry| entry.memory.as_ref())
        .unwrap();
    assert!(claimed_memory.enabled);
    assert_eq!(claimed_memory.deliveries[0].model, SOL_MODEL);
    assert_eq!(claimed_memory.deliveries[0].block.model[0].text, OWNER_NOTE);

    set_crew_memory_enabled(&pair.follower, false);
    assert!(owner.current_crew_memory_enabled());
    let codex_dir = tempfile::tempdir().unwrap();
    let (program, stdin_path) = install_fake_codex(codex_dir.path());
    // SAFETY: this scenario row owns the process environment; RowHome restores
    // it before the next row. The override points both runtimes at the fake.
    unsafe { std::env::set_var("ORBIT_V2_CLI_CODEX", &program) };
    point_codex_at(owner, &program);
    point_codex_at(&pair.follower, &program);

    let withheld = bind_leaf(&pair, &leaf, Arc::new(DropMemoryPolicy(owner.clone())));
    let withheld_envelope = dispatch_envelope(&withheld, &leaf, task, &stdin_path);
    assert!(
        withheld_envelope.get("crew_memory").is_none(),
        "a missing owner observation delivers no memory: {withheld_envelope}"
    );

    let bound = bind_leaf(&pair, &leaf, Arc::new(ToOwner(owner.clone())));
    let delivered = dispatch_envelope(&bound, &leaf, task, &stdin_path);
    assert_eq!(
        delivered
            .pointer("/crew_memory/model/0/text")
            .and_then(Value::as_str),
        Some(OWNER_NOTE),
        "the leaf delivers the captured owner block while its own key is off: {delivered}"
    );

    set_crew_memory_enabled(owner, false);
    let suppressed = dispatch_envelope(&bound, &leaf, task, &stdin_path);
    assert!(
        suppressed.get("crew_memory").is_none(),
        "the owner disable suppresses the next claimed envelope: {suppressed}"
    );
    let owner_envelope = dispatch_envelope(owner, &owner_run.run_id, task, &stdin_path);
    assert!(
        owner_envelope.get("crew_memory").is_none(),
        "the owner disable suppresses the owner dispatch the same way: {owner_envelope}"
    );

    assert_eq!(
        pair.admission(&leaf).receipt.unwrap().experiments,
        Some(receipt_experiments),
        "disabling crew memory leaves the captured receipt usable and unchanged"
    );
    assert_eq!(
        owner
            .show_job_run(&owner_run.run_id)
            .unwrap()
            .input
            .unwrap()["experiment_capture"],
        owner_capture,
        "the owner run keeps the capture it froze while the key was on"
    );
}

crate::scenario_table!(
    experiment_admission_scenarios,
    ended_experiments_change_fresh_claims_but_not_captured_workers,
    owner_disable_suppresses_claimed_memory_delivery
);
