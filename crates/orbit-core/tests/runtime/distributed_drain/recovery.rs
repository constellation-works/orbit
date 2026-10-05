//! Final recovery of claimed leaves.

use super::*;

/// [ORB-13964] The owner can execute final recovery after a follower settles
/// its failed claim, without importing the follower's run or pipeline state.
#[test]
#[cfg(unix)]
fn a_settled_follower_failure_is_recovered_with_the_recovery_runs_own_crew_draw() {
    use std::os::unix::fs::PermissionsExt;

    use orbit_core::application::task::{
        BLOCKED_TASK_RECOVERY_JOB, BlockedRecoveryInput, EpisodeDisposition, FinalRecoveryRecord,
        FinalRecoveryTaskRevision,
    };
    use orbit_types::workflow::{ExecutorSandboxKind, FINAL_RECOVERY_CREWS_KEY};

    if !isolated(
        module_path!(),
        "a_settled_follower_failure_is_recovered_with_the_recovery_runs_own_crew_draw",
    ) {
        return;
    }
    let pair = Pair::with_crews(&[Some("sol")]);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task_id = pair.claimed_task(&leaf);
    let failure = "validation failed; preserved candidate branch: fixture/candidate";
    pair.leaf_fails_with(&leaf, failure);
    pair.pass(&drain);
    assert_eq!(pair.owner_status(&task_id), "blocked");
    assert_eq!(pair.owner_claims()[0]["claim"]["phase"], "failed");

    let owner_repo = pair.wire.owner.paths().repo_root.clone();
    std::fs::write(owner_repo.join(".gitignore"), ".orbit/\n").unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["add", ".gitignore", "src"],
        vec![
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@orbit.invalid",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "base",
        ],
        vec!["branch", "fixture/candidate"],
    ] {
        let output = orbit_common::fs::git::run_git(&owner_repo, &args).expect("seed owner Git");
        assert!(output.success, "{args:?}: {}", output.stderr);
    }
    std::fs::write(
        owner_repo.join(".orbit/config.toml"),
        r#"[workflow]
default_crew = "recovery_a"
final_recovery_crews = ["recovery_a:7", "recovery_b:3"]

[crews.sol]
provider = "codex"
model = "fixture-task"

[crews.recovery_a]
provider = "codex"
model = "fixture-a"

[crews.recovery_b]
provider = "codex"
model = "fixture-b"
"#,
    )
    .unwrap();
    orbit_core::bootstrap::init::init_workspace_at_root(
        &pair.wire.owner.global_root(),
        orbit_core::bootstrap::init::InitOptions {
            global_only: true,
            refresh_defaults: true,
            ..Default::default()
        },
    )
    .expect("seed shipped recovery job and activities");
    let owner =
        OrbitRuntime::from_roots(&pair.wire.owner.global_root(), &owner_repo.join(".orbit"))
            .unwrap()
            .with_automation_machine_identity(Some(OWNER.into()));
    assert!(owner.read_run_state(&leaf).unwrap().is_none());
    let jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    assert!(jobs.get_job_run(&leaf).unwrap().is_none());

    // The real dispatcher selects the crew and injects execution identity;
    // only the provider's decision is deterministic in this fixture.
    let provider = pair._root.path().join("codex");
    std::fs::write(
        &provider,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"decision\":\"requeue\",\"reason\":\"fixture prerequisite repaired\"},\"error\":null}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o755)).unwrap();
    follower_cli(&owner, "codex", "sh");
    let mut executor = owner.get_executor_def("codex").unwrap().unwrap();
    executor.command = Some(provider.to_string_lossy().to_string());
    executor.sandbox = Some(ExecutorSandboxKind::Off);
    owner.upsert_executor_def(&executor).unwrap();

    let views = owner.blocked_recovery_view(Utc::now()).unwrap();
    assert_eq!(views.len(), 1);
    let view = &views[0];
    assert_eq!(view.disposition, EpisodeDisposition::Eligible);
    assert_eq!(view.episode.failed_run_id.as_deref(), Some(leaf.as_str()));
    let input = BlockedRecoveryInput {
        task_id: task_id.clone(),
        episode_key: view.episode.key(),
        block_source: view.episode.source.as_str().to_string(),
        failed_run_id: view.episode.failed_run_id.clone(),
        observed: FinalRecoveryTaskRevision::of(&owner.get_task(&task_id).unwrap()),
    };
    let job = owner
        .show_job_catalog_entry(BLOCKED_TASK_RECOVERY_JOB)
        .unwrap();
    let recovery = owner
        .run_job_v2_from_yaml(&job.path, input.to_json())
        .unwrap();
    assert!(recovery.success, "{recovery:#?}");
    let prepared = &recovery.pipeline["prepare"]["recovery"];
    assert_eq!(prepared["run_id"], leaf);
    assert!(
        prepared["error_message"]
            .as_str()
            .unwrap()
            .contains(failure),
        "the owner supplies the settled diagnostic, including available candidate evidence: {prepared}"
    );
    assert!(!Path::new(prepared["workspace_path"].as_str().unwrap()).exists());
    let state = owner.read_run_state(&recovery.run_id).unwrap().unwrap();
    let frozen = &state.activity_crew_draws[FINAL_RECOVERY_CREWS_KEY];
    assert_eq!(
        frozen
            .eligible_pool
            .iter()
            .map(|member| (member.name.as_str(), member.weight))
            .collect::<Vec<_>>(),
        [("recovery_a", 7), ("recovery_b", 3)]
    );
    let config = owner
        .agent_crew_config_for_input(&json!({
            "run_id": leaf,
            "job_run_id": recovery.run_id,
            "crew_config_key": FINAL_RECOVERY_CREWS_KEY,
        }))
        .unwrap()
        .unwrap();
    assert_eq!(
        config,
        owner
            .agent_crew_config_for_input(&json!({"crew": frozen.crew}))
            .unwrap()
            .unwrap()
    );
    assert_eq!(
        owner.get_task(&task_id).unwrap().status.to_string(),
        "backlog"
    );
    let record = FinalRecoveryRecord::last(&owner.get_task_comments(&task_id).unwrap()).unwrap();
    assert_eq!(record.run_id, recovery.run_id);
    assert_eq!(record.decision, "requeue");
    assert_eq!(record.outcome, "requeued");
    assert!(owner.read_run_state(&leaf).unwrap().is_none());
    assert!(jobs.get_job_run(&leaf).unwrap().is_none());
    let candidate =
        orbit_common::fs::git::run_git(&owner_repo, &["rev-parse", "fixture/candidate"]).unwrap();
    assert!(candidate.success, "candidate evidence is preserved");
    assert_eq!(
        candidate.stdout.trim(),
        prepared["base_sha"].as_str().unwrap()
    );
}

/// [ORB-13907] A claimed leaf's final-recovery decision never touches the
/// owner's task from the follower: it rides on the leaf's failure settlement,
/// and the owner applies it once its journal has failed the claim. A replayed
/// settlement changes nothing more.
#[test]
fn a_claimed_leaf_final_recovery_decision_is_applied_by_the_owner_through_settlement() {
    if !isolated(
        module_path!(),
        "a_claimed_leaf_final_recovery_decision_is_applied_by_the_owner_through_settlement",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    let mut state = pair
        .follower_jobs
        .read_run_state(&leaf)
        .unwrap()
        .unwrap_or_else(|| PipelineState::new(leaf.clone(), LEAF_JOB.into(), json!({})));
    state.final_recovery = Some(FinalRecoveryCheckpoint {
        key: FinalRecoveryKey {
            run_id: leaf.clone(),
            attempt: 1,
        },
        failed_step_id: "implement".into(),
        task_id: task.clone(),
        observed: None,
        base_ref: Some("main".into()),
        admitted_at: Utc::now(),
        decision: Some(FinalRecoveryDecision::Reject {
            reason: "the task contradicts its own criteria".into(),
            evidence: "criterion 1 forbids the change criterion 2 requires".into(),
        }),
        outcome: Some("settled: recorded for the claim settlement".into()),
    });
    pair.follower.write_run_state(&leaf, &state).unwrap();
    // The leaf ran, its final recovery decided, and the run failed.
    pair.leaf_fails_with(&leaf, "implement failed");
    let before = pair.owner_task(&task);

    let settled = pair.pass(&drain);
    assert!(
        !launch_refused(&settled),
        "a failed leaf is settled, not relaunched: {settled}"
    );
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert_eq!(settles.len(), 1, "{settles:?}");
    let carried = &settles[0]["settlement"]["Fail"]["final_recovery"];
    assert_eq!(carried["run_id"], leaf.as_str(), "{settles:?}");
    assert_eq!(carried["decision"]["decision"], "reject", "{settles:?}");
    assert_ne!(
        before["status"], "rejected",
        "the follower wrote nothing to the owner's task"
    );

    assert_eq!(pair.owner_claims()[0]["claim"]["phase"], "failed");
    let applied = pair.owner_task(&task);
    assert_eq!(applied["status"], "rejected", "{applied:#}");
    let header = format!("final_recovery run_id={leaf} decision=reject");
    let recorded = |task: &Value| {
        task["comments"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|comment| {
                comment["message"]
                    .as_str()
                    .is_some_and(|message| message.starts_with(&header))
            })
            .count()
    };
    assert_eq!(recorded(&applied), 1, "{applied:#}");

    let replay = pair
        .wire
        .call("", "orbit.drain.claim.settle", settles[0].clone())
        .expect("a replayed settlement answers with the recorded outcome");
    assert_eq!(replay["phase"], "failed", "{replay}");
    let after = pair.owner_task(&task);
    for field in ["status", "comments", "history"] {
        assert_eq!(after[field], applied[field], "{field} changed on replay");
    }
}
