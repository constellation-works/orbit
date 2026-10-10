//! The approval drain's pilot consumes the implementer's required-tool contract
//! without granting tools its shipped policy disallows. The real pilot job and
//! runtime run against a scripted provider in an isolated fixture [ORB-15207].

use std::os::unix::fs::PermissionsExt;

use orbit_engine::activity_job::{load_activity_asset, load_job_asset};
use orbit_engine::{
    V2AuditWriter, execute_job_with_resume, resolve_job_catalog_refs_for_execution,
};
use orbit_types::resource::ExecutorResource;
use orbit_types::workflow::activity_job::V2AuditEventKind;
use orbit_types::workflow::{ExecutorDef, ExecutorSandboxKind};

use super::*;

#[test]
fn approval_drain_runs_a_pilot_with_a_disallowed_implementer_requirement() {
    if !super::super::dispatch_admission::isolated(
        "drain_approval::required_tools::approval_drain_runs_a_pilot_with_a_disallowed_implementer_requirement",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let task = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "Review requiring task creation".into(),
            description: "The implementer files review findings as tasks.".into(),
            acceptance_criteria: vec!["Review findings are filed.".into()],
            plan: "Review README.md and file findings.".into(),
            context_files: vec!["file:README.md".into()],
            complexity: TaskComplexity::Low,
            required_tools: vec!["orbit.task.add".into()],
            ..Default::default()
        })
        .unwrap();
    let drain = workspace.running("workspace_auto_pipeline", json!({"approve_proposed": true}));
    let selection = workspace.select(&drain);
    assert_eq!(selection["task_ids"], json!([task.id]));
    let pilot = workspace.pilot_child(&drain, &selection["task_ids"]);

    // Only the provider response is scripted: admission, preparation, fan-out,
    // result application and drain promotion use the shipped job and runtime.
    let provider = workspace._root.path().join("codex");
    let response = json!({
        "schemaVersion": 1, "status": "success", "error": null,
        "result": {"partition_index": 0, "task_ids": [task.id], "tasks": [assessment(&task)]},
    });
    std::fs::write(
        &provider,
        format!("#!/bin/sh\ncat > /dev/null\ncat <<'RESPONSE'\n{response}\nRESPONSE\n"),
    )
    .unwrap();
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o755)).unwrap();
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let mut executor: ExecutorResource = serde_yaml::from_str(
        &std::fs::read_to_string(assets.join("executors/codex.yaml")).unwrap(),
    )
    .unwrap();
    executor.spec.command = Some(provider.display().to_string());
    // This disposable provider tests admission, independently of OS confinement.
    executor.spec.sandbox = Some(ExecutorSandboxKind::Off);
    workspace
        .runtime
        .upsert_executor_def(&ExecutorDef::from_resource_spec(
            executor.metadata.name,
            executor.spec,
            None,
            None,
        ))
        .unwrap();

    let mut catalog = workspace.runtime.v2_activity_catalog().unwrap();
    let guard = load_activity_asset(
        &std::fs::read_to_string(assets.join("activities/pipeline_success_guard.yaml")).unwrap(),
    )
    .unwrap();
    catalog.insert(guard.name, guard.spec);
    let mut job = load_job_asset(
        &std::fs::read_to_string(assets.join("jobs/task_pilot_pipeline.yaml")).unwrap(),
    )
    .unwrap()
    .spec;
    resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
    let audit = V2AuditWriter::with_disk_sinks(
        &workspace._root.path().join("audit"),
        workspace.runtime.v2_audit_store().unwrap(),
        workspace.runtime.workspace_id().unwrap(),
        &pilot,
        "codex",
        Some(&workspace.repo),
    )
    .unwrap();
    let outcome = execute_job_with_resume(
        &job,
        json!({
            "task_ids": selection["task_ids"], "workspace_path": workspace.repo,
            "base_branch": "main", "promotion_authorized": true,
            "drain_promotion": selection["drain_promotion"],
        }),
        &pilot,
        audit.clone(),
        &workspace.runtime,
        None,
    )
    .expect("ORB-15207: the approval drain's pilot must admit implementer tool requirements");
    assert!(outcome.success, "the pilot job succeeds: {outcome:?}");
    assert_eq!(outcome.pipeline["prepare"]["task_count"], 1);
    assert_eq!(outcome.pipeline["apply"]["status"], "succeeded");
    assert_eq!(workspace.status(&task), TaskStatus::Backlog);
    assert_eq!(
        workspace.runtime.get_task(&task.id).unwrap().required_tools,
        ["orbit.task.add"],
        "piloting preserves the implementer's contract"
    );

    let events = audit.events_snapshot().unwrap();
    let (requested, effective, notes) = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::ToolAllowlistHarnessDelegated {
                task_ids,
                requested_tools,
                effective_tools,
                omitted_requirement_notes,
                ..
            } if task_ids == std::slice::from_ref(&task.id) => {
                Some((requested_tools, effective_tools, omitted_requirement_notes))
            }
            _ => None,
        })
        .expect("the pilot reached the provider with its admitted tool policy");
    assert!(!requested.iter().any(|tool| tool == "orbit.task.add"));
    assert!(!effective.iter().any(|tool| tool == "orbit.task.add"));
    assert!(
        notes.iter().any(|note| {
            note.contains(&task.id)
                && note.contains("orbit.task.add")
                && note.contains("task_pilot")
        }),
        "the omitted pilot requirement is recorded: {notes:?}"
    );
    let jobs = orbit_store::compose::workspace_job_run_store(
        workspace.runtime.sqlite_store().unwrap(),
        workspace.runtime.workspace_id().unwrap(),
    );
    jobs.finalize_job_run(&pilot, JobRunState::Success, Utc::now(), None)
        .unwrap();
}
