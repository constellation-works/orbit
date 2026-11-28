//! Dashboard triage is decoration: an unreadable job store does not fail the task.

use orbit_types::task::TaskStatus;

use super::task::{enter_isolated_child, test_runtime};
use crate::application::task::write_response::{
    DashboardPageReads, TaskWriteResponsePolicy, task_detail_with_dashboard_extras,
    task_write_response_json,
};
use crate::application::task::{TaskAddParams, TaskWriteMetadata};

#[test]
fn unreadable_job_runs_leave_dashboard_task_detail_without_triage() {
    if !enter_isolated_child(
        module_path!(),
        "unreadable_job_runs_leave_dashboard_task_detail_without_triage",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Dashboard task".into(),
            description: "Shown without triage badges.".into(),
            acceptance_criteria: vec!["The record still loads.".into()],
            status: Some(TaskStatus::Proposed),
            ..Default::default()
        })
        .expect("add task");
    let database = runtime.global_root().join("orbit.db");
    let connection = rusqlite::Connection::open(&database).expect("open store");
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .expect("busy timeout");
    connection
        .execute_batch("ALTER TABLE job_runs RENAME TO job_runs_unreadable;")
        .expect("make job-run listing fail");

    let row = runtime
        .task_row_with_sidecars(task.clone())
        .expect("task row");
    let detail = task_detail_with_dashboard_extras(
        &runtime,
        &row,
        &Default::default(),
        &DashboardPageReads::default(),
    )
    .expect("detail degrades when job runs cannot be listed");
    assert!(
        detail.get("active_triage").is_none(),
        "no triage binding is invented: {detail}"
    );
    assert_eq!(detail["id"].as_str(), Some(task.id.as_str()));

    let response = task_write_response_json(
        &runtime,
        &task,
        &TaskWriteResponsePolicy::DashboardFullRecord,
        &TaskWriteMetadata::default(),
    )
    .expect("a dashboard write response degrades the same way");
    assert!(response.get("active_triage").is_none(), "{response}");
    assert_eq!(response["id"].as_str(), Some(task.id.as_str()));
}

#[test]
fn blocked_open_decisions_for_handoff_landing_stay_off_commit_boundary() {
    if !enter_isolated_child(
        module_path!(),
        "blocked_open_decisions_for_handoff_landing_stay_off_commit_boundary",
    ) {
        return;
    }
    use crate::application::task::{TaskRecordUpdateParams as StoreTaskUpdateParams, TaskRecords};
    use chrono::Utc;
    use orbit_engine::{RunLifecycle, TaskAutomationUpdate};
    use orbit_store::contract::TaskArtifactUpdateParams;
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
    use orbit_types::task::{
        ArtifactWriter, DecisionAction, DecisionBinding, DecisionGate, DecisionOptionInput,
        DecisionRequest, DecisionRewrite, ExecutionLocation, TaskArtifact, decision_option_answers,
    };
    use orbit_types::workflow::automation::SourceRevision;
    use orbit_types::workflow::{
        CommitIdentity, FailureActivityCheckpoint, JobRunState, PipelineState,
        REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT, ReviewBudget, ReviewCertificate,
        ReviewConsumption, ReviewVerdict, ReviewerIdentity,
    };
    use serde_json::json;

    let (root, base_runtime) = test_runtime();
    let config_path = base_runtime.paths().orbit_dir.join("config.toml");
    let mut config = std::fs::read_to_string(&config_path).expect("read config");
    config.push_str("\n[review]\ntiming = \"before-landing\"\n");
    std::fs::write(&config_path, config).expect("write review config");
    let runtime = crate::OrbitRuntime::from_roots(
        &base_runtime.global_root(),
        &base_runtime.paths().orbit_dir,
    )
    .expect("reload runtime")
    .with_automation_machine_identity(Some("hm_fixture".into()));
    drop(base_runtime);

    let task = runtime
        .add_task(TaskAddParams {
            title: "Blocked handoff landing task".into(),
            description: "Verify observational BlockTriage handoff landing.".into(),
            acceptance_criteria: vec!["Stay off task-commit.".into()],
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("add task");

    let host = crate::adapter::engine_host::EngineHost::new(&runtime);
    let now = Utc::now();
    let run = host
        .insert_job_run(
            "task_pr_pipeline",
            1,
            now,
            Some(json!({"task_ids": [task.id], "base_branch": "main", "completion": "review"})),
            None,
        )
        .expect("insert run");
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn exited worker");
    let dead_pid = child.id();
    let _ = child.wait().expect("reap exited worker");
    host.mark_job_run_running(&run.run_id, now, dead_pid)
        .expect("start run");
    host.finalize_job_run(&run.run_id, JobRunState::Failed, now, Some(1))
        .expect("fail run");
    let head_sha = "a".repeat(40);
    let base_sha = "b".repeat(40);
    let mut state = PipelineState::new(
        run.run_id.clone(),
        run.job_id.clone(),
        run.input.clone().unwrap(),
    );
    state.failure_activity_checkpoint = Some(FailureActivityCheckpoint {
        failed_step_id: "validate".into(),
        activity_name: "pr_failure_handoff".into(),
        output: json!({
            "decision": "blocked_failure_pr",
            "pr_number": "17",
            "branch": format!("orbit/{}-handoff", task.id),
            "head_sha": head_sha,
            "task_spec_digest": task.spec_digest(),
        }),
    });
    runtime
        .write_run_state(&run.run_id, &state)
        .expect("write run state");

    let answers = decision_option_answers(
        &[
            DecisionOptionInput {
                summary: "Narrow the task".into(),
                consequence: "Narrow the task lands.".into(),
                rewrite: Some(DecisionRewrite {
                    description: Some("Rewritten task description.".into()),
                    acceptance_criteria: None,
                }),
                action: None,
            },
            DecisionOptionInput {
                summary: "Resume the candidate as it is".into(),
                consequence: "Resume the candidate lands.".into(),
                rewrite: None,
                action: Some(DecisionAction::Requeue),
            },
        ],
        1,
    )
    .expect("decision options");
    host.apply_task_automation_update(
        &task.id,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Blocked),
            status_note: Some("blocked with handoff PR".into()),
            job_run_id: Some(run.run_id.clone()),
            ..Default::default()
        },
    )
    .expect("block task");
    let blocked_at = runtime
        .get_task_history(&task.id)
        .expect("task history")
        .into_iter()
        .rfind(|entry| entry.to_status == Some(TaskStatus::Blocked))
        .expect("blocked status history entry")
        .at;
    let decision = DecisionRequest::new(
        task.id.clone(),
        DecisionGate::BlockTriage,
        answers,
        DecisionBinding::BlockEpisode {
            episode: blocked_at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
        },
        "block_triage".into(),
        now,
    );
    let request_id = decision.id.clone();
    let task = runtime
        .stores()
        .task_records()
        .update(
            &task.id,
            StoreTaskUpdateParams {
                job_run_machine: Some(Some(ExecutionLocation {
                    machine_id: "hm_fixture".into(),
                    machine_name: Some("fixture".into()),
                })),
                actor: "system".into(),
                append_history: vec![decision.history_entry("system")],
                ..Default::default()
            },
        )
        .expect("record block triage and job run machine");

    let meaning_digest = orbit_automation::review::combined_task_meaning_digest(&[(
        task.id.clone(),
        orbit_automation::review::task_meaning_digest(&task).expect("task digest"),
    )])
    .expect("combined digest");
    let cert = ReviewCertificate {
        schema_version: REVIEW_CONTRACT_VERSION,
        attempt_id: format!("rev-attempt-{}", task.id),
        lineage_key: format!("lineage-{}", task.id),
        task_ids: vec![task.id.clone()],
        task_meaning_digest: meaning_digest,
        repository: "owner/repo".into(),
        base: SourceRevision {
            commit: base_sha,
            tree: "c".repeat(40),
        },
        reviewed_candidate: SourceRevision {
            commit: head_sha.clone(),
            tree: "d".repeat(40),
        },
        final_candidate: SourceRevision {
            commit: head_sha,
            tree: "d".repeat(40),
        },
        implementation_commits: Vec::<CommitIdentity>::new(),
        repair_commits: Vec::<CommitIdentity>::new(),
        verdict: ReviewVerdict::Accept,
        assurance: None,
        findings: Vec::new(),
        validation: Vec::new(),
        required_validation_commands: None,
        baseline_commands: Vec::new(),
        validation_complete: true,
        retained_obligations: Vec::new(),
        retired_validation: Vec::new(),
        validation_scope: Vec::new(),
        reviewer: ReviewerIdentity {
            crew: "sol".into(),
            provider: "codex".into(),
            model: "test".into(),
            reasoning_effort: None,
            implementer_model: None,
            same_model_as_implementer: false,
        },
        consumed: ReviewConsumption { seconds: 1 },
        budget: ReviewBudget { minutes: 10 },
        escalation: None,
        selectors_widened: Vec::new(),
        evidence_carried: None,
        baseline_red: Vec::new(),
        host_overrides: Vec::new(),
        known_red: Vec::new(),
        host_evidence: Vec::new(),
        owed_evidence: Vec::new(),
        owner_owed_evidence: Vec::new(),
        host_validation: Vec::new(),
        resumed_hold_attempt: None,
        issued_at: now,
    };
    let ws = runtime.workspace_id().expect("workspace id");
    runtime
        .sqlite_store()
        .expect("sqlite store")
        .write(|tx| crate::application::review::ledger::record_certificate(tx, &ws, &cert))
        .expect("record cert");
    runtime
        .stores()
        .task_artifacts()
        .upsert_task_artifacts(
            &task.id,
            TaskArtifactUpdateParams {
                actor: "system".into(),
                writer: Some(ArtifactWriter::System),
                upsert_artifacts: vec![TaskArtifact::from_text(
                    REVIEW_GATE_ARTIFACT,
                    serde_json::to_string(&cert).expect("cert json"),
                )],
                ..Default::default()
            },
        )
        .expect("upsert gate artifact");

    let registry = TaskRegistryStore::open(&task_registry_path(&root.path().join("global")))
        .expect("open registry");
    let binding = registry
        .find_task_binding(&task.id)
        .expect("lookup")
        .expect("binding");
    let partition = binding.canonical_path.parent().expect("partition");
    let connection =
        rusqlite::Connection::open(runtime.global_root().join("orbit.db")).expect("db");
    connection
        .execute(
            "INSERT INTO task_commit_journal \
             (journal_id, workspace_id, task_id, state, intent_json, created_at) \
             VALUES ('blocked-handoff-landing', ?1, ?2, 'prepared', '{}', ?3)",
            rusqlite::params![ws, task.id, now.to_rfc3339()],
        )
        .expect("insert journal");

    orbit_common::fs::io::with_exclusive_file_lock::<_, orbit_common::OrbitError, _>(
        &partition.join("task-commit"),
        "observational blocked handoff landing open_decisions",
        || {
            let row = runtime.observe_task_row(&task.id)?;
            let open = runtime.observe_open_decisions_json_in(
                &row.task,
                &row.history,
                &row.comments,
                &row.artifacts,
            )?;
            let card = open
                .as_array()
                .expect("open decisions array")
                .iter()
                .find(|entry| entry["id"] == request_id)
                .expect("BlockTriage card");
            assert!(
                card["answers"]
                    .as_array()
                    .expect("answers")
                    .iter()
                    .any(|answer| answer == "land"),
                "derived land answer must resolve from observational REVIEW_GATE_ARTIFACT: {card}"
            );
            let rewrite = card["options"]
                .as_array()
                .expect("options")
                .iter()
                .find(|option| option["id"] == "1")
                .expect("rewrite option");
            assert!(
                rewrite["label"]
                    .as_str()
                    .is_some_and(|label| label.contains("candidate is discarded")),
                "rewrite option must disclose discard from observational state: {rewrite}"
            );
            Ok(())
        },
    )
    .expect("observational read under held task-commit");

    let state = || {
        connection
            .query_row(
                "SELECT state FROM task_commit_journal WHERE journal_id = 'blocked-handoff-landing'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("journal state")
    };
    assert_eq!(state(), "prepared");
    runtime.get_task(&task.id).expect("coordinated read");
    assert_eq!(state(), "aborted");
}
