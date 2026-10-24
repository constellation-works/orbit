//! Drain approval decides at its own lock: an operator write that wins the
//! race between the pilot write and the approval transition is honoured.

use chrono::Utc;
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::{Task, TaskComplexity, TaskStatus};
use orbit_types::workflow::{ChildDispatch, PipelineState};
use serde_json::{Value, json};

use super::pilot_persist::{Workspace, workspace};
use orbit_core::OrbitRuntime;
use orbit_core::application::task::TaskAddParams;

/// A running run of `job`, as the drain and its pilot child are.
fn running(runtime: &OrbitRuntime, job: &str, input: Value) -> String {
    let jobs = crate::admission_audit::store_support::jobs(runtime);
    let run = jobs
        .insert_job_run(job, 1, Utc::now(), Some(input), None)
        .expect("insert run");
    runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id, json!({})),
        )
        .expect("write run state");
    jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("mark run running");
    run.run_id
}

/// An `--approve-proposed` drain and the detached pilot child it dispatched.
fn drain_with_pilot(runtime: &OrbitRuntime, task_id: &str) -> (String, String) {
    let drain = running(
        runtime,
        "workspace_auto_pipeline",
        json!({"approve_proposed": true}),
    );
    let pilot = running(
        runtime,
        "task_pilot_pipeline",
        json!({"task_ids": [task_id]}),
    );
    let mut state = runtime
        .read_run_state(&drain)
        .expect("read drain state")
        .expect("drain state");
    state.record_child_dispatch(ChildDispatch::submitted(
        pilot.clone(),
        "task_pilot_pipeline".into(),
        "invoke_detached".into(),
        false,
        false,
        Utc::now(),
    ));
    runtime
        .write_run_state(&drain, &state)
        .expect("record pilot dispatch");
    (drain, pilot)
}

fn qualifying_task(runtime: &OrbitRuntime) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: "scoped".to_string(),
            description: "Ship the README change.".to_string(),
            acceptance_criteria: vec!["The change is in place.".to_string()],
            plan: "Edit README.md.".to_string(),
            status: Some(TaskStatus::Proposed),
            context_files: vec!["file:README.md".to_string()],
            complexity: TaskComplexity::Low,
            ..Default::default()
        })
        .expect("add task")
}

fn action(runtime: &OrbitRuntime, action: &str, input: Value) -> Value {
    runtime
        .run_deterministic(action, &json!({}), &input, ToolContext::default())
        .unwrap_or_else(|error| panic!("{action}: {error}"))
}

/// Prepare and apply a clean assessment under the drain's authority, with
/// `race` run just before the approval takes its locks.
fn pilot_with_race(
    workspace: &Workspace,
    task: &Task,
    drain: &str,
    pilot: &str,
    _race: Option<()>,
) -> Value {
    pilot_with_rationale(
        workspace,
        task,
        drain,
        pilot,
        _race,
        "README.md holds the change.",
    )
}

fn pilot_with_rationale(
    workspace: &Workspace,
    task: &Task,
    drain: &str,
    pilot: &str,
    _race: Option<()>,
    assessment_rationale: &str,
) -> Value {
    pilot_with_findings(
        workspace,
        task,
        drain,
        pilot,
        _race,
        json!({
            "assessment_rationale": assessment_rationale,
        }),
    )
}

fn pilot_with_findings(
    workspace: &Workspace,
    task: &Task,
    drain: &str,
    pilot: &str,
    _race: Option<()>,
    findings: Value,
) -> Value {
    let runtime = &workspace.runtime;
    let prepared = action(
        runtime,
        "prepare_task_pilot",
        json!({"task_ids": [task.id], "workspace_path": workspace.repo, "base_branch": "main"}),
    );
    let mut assessment = json!({
        "task_id": task.id,
        "context_files_before": task.context_files,
        "context_files_after": ["file:README.md"],
        "disposition": "selectors", "recommended_crew": "opus",
        "recommended_complexity": "low", "confidence": "high",
        "assessment_rationale": "README.md holds the change.",
        "validation_approach": "Inspect README.md.",
        "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
        "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
        "duplicate_of": null, "already_landed": null,
    });
    for (field, value) in findings.as_object().expect("findings") {
        assessment[field] = value.clone();
    }

    // The fixture pilot read its checkout: report the observed pair of
    // the prepared revision and its tree.
    let revision = prepared["source"]["source_revision"]
        .as_str()
        .expect("prepared source revision")
        .to_string();
    let tree = {
        let output = crate::git_test_support::command()
            .args(["rev-parse", &format!("{revision}^{{tree}}")])
            .current_dir(&workspace.repo)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    let applied = runtime.run_deterministic(
        "apply_task_pilot_results",
        &json!({}),
        &json!({
            "run_id": pilot,
            "workspace_path": workspace.repo,
            "prepared": prepared,
            "results": [{
                "partition_index": 0, "observed_revision": revision, "observed_tree": tree,
                "task_ids": [task.id], "tasks": [assessment],
            }],
            "promotion_authorized": true,
            "drain_promotion": {"run_id": drain},
        }),
        ToolContext::default(),
    );

    let applied = applied.expect("apply task pilot results");
    assert_eq!(applied["status"], "succeeded", "{applied}");
    applied
}

#[test]
fn unchanged_task_gets_exactly_one_ordinary_approval() {
    if !super::isolate(
        module_path!(),
        "unchanged_task_gets_exactly_one_ordinary_approval",
    ) {
        return;
    }

    let workspace = workspace(None);
    let runtime = &workspace.runtime;
    let task = qualifying_task(runtime);
    let (drain, pilot) = drain_with_pilot(runtime, &task.id);

    let applied = pilot_with_race(&workspace, &task, &drain, &pilot, None);

    let decision = &applied["drain_approval"][0];
    assert_eq!(decision["decision"], "promote", "{decision}");
    assert_eq!(decision["approved"], true, "{decision}");
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::Backlog
    );
    let history = runtime.get_task_history(&task.id).expect("history");
    let approvals = history
        .iter()
        .filter(|entry| entry.event == "proposal_approved")
        .collect::<Vec<_>>();
    assert_eq!(approvals.len(), 1, "{history:?}");
    assert!(
        approvals[0]
            .note
            .as_deref()
            .is_some_and(|note| note.contains(&drain)),
        "the approval names the drain: {approvals:?}"
    );
}
