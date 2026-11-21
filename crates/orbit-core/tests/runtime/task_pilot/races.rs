//! Admission and operator interleavings through the composed runtime.

use orbit_core::TaskComplexity;
use orbit_core::application::task::TaskUpdateParams;

use super::*;

fn assessment(task: &Value) -> Value {
    json!({
        "task_id": task["task_id"],
        "context_files_before": task["context_files_before"],
        "context_files_after": ["file:README.md"],
        "disposition": "selectors", "recommended_crew": "fixture",
        "recommended_complexity": "low", "confidence": "high",
        "assessment_rationale": "README.md contains the affected material.",
        "validation_approach": "Inspect the persisted assessment.",
        "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
        "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
        "duplicate_of": null, "already_landed": null,
    })
}

pub(super) fn apply_input(workspace: &Workspace, prepared: &Value) -> Value {
    json!({
        "workspace_path": workspace.repo,
        "prepared": prepared,
        "results": prepared["partitions"].as_array().unwrap().iter().map(|partition| {
            let tasks = prepared["tasks"].as_array().unwrap().iter()
                .filter(|task| partition["task_ids"].as_array().unwrap().contains(&task["task_id"]))
                .map(assessment).collect::<Vec<_>>();
            json!({"partition_index": partition["partition_index"], "task_ids": partition["task_ids"], "tasks": tasks})
        }).collect::<Vec<_>>(),
    })
}

fn assert_superseded(workspace: &Workspace, output: &Value, count: usize) {
    assert_eq!(output["status"], "succeeded", "{output}");
    assert_eq!(output["outcome"], "superseded", "{output}");
    assert_eq!(output["superseded_count"], count, "{output}");
    assert_eq!(output["unresolved_count"], 0, "{output}");
    assert!(output["error"].is_null(), "{output}");
    assert_eq!(
        workspace.action("pipeline_success_guard", json!({"result": output}))["succeeded"],
        true
    );
}

fn admitted_and_retired_targets_settle_as_superseded_without_pilot_writes() {
    let workspace = Workspace::new();
    let tasks = [
        "admitted",
        "implementer plan",
        "rejected",
        "archived",
        "someday",
    ]
    .map(|title| workspace.task(title));
    for task in &tasks[..2] {
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                },
                "fixture".into(),
            )
            .unwrap();
    }
    let ids = tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>();
    let prepared = workspace.prepare(&ids);
    for task in &tasks[..2] {
        workspace
            .runtime
            .admit_task_for_workflow(&task.id, "worktree_setup")
            .unwrap();
    }
    workspace
        .runtime
        .update_task_with_identity(
            &tasks[1].id,
            TaskUpdateParams {
                plan: Some("The admitted implementer's concrete plan.".into()),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();
    for (task, status) in tasks[2..].iter().zip([
        TaskStatus::Rejected,
        TaskStatus::Archived,
        TaskStatus::Someday,
    ]) {
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    status: Some(status),
                    ..Default::default()
                },
                "fixture".into(),
            )
            .unwrap();
    }
    let before = ids
        .iter()
        .map(|id| workspace.runtime.get_task(id).unwrap())
        .collect::<Vec<_>>();
    let mut input = apply_input(&workspace, &prepared);
    // The operator's rejection can also make the agent's reported snapshot
    // stale. No metadata from this assessment may be written [ORB-14297].
    input["results"][0]["tasks"][2]["context_files_before"] = json!(["file:other.rs"]);
    let output = workspace.action("apply_task_pilot_results", input);
    assert_superseded(&workspace, &output, tasks.len());
    assert_eq!(output["applied_count"], 0);
    for task in &before {
        assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), *task);
        assert!(
            workspace
                .runtime
                .get_task_history(&task.id)
                .unwrap()
                .iter()
                .all(|entry| entry.event != "task_pilot_applied")
        );
    }
}

fn human_material_edits_supersede_preparation_without_writes() {
    let workspace = Workspace::new();
    for admitted in [false, true] {
        for field in ["description", "plan", "context"] {
            let task = workspace.task(&format!("edit {field} admitted {admitted}"));
            workspace
                .runtime
                .update_task_as_human(
                    &task.id,
                    TaskUpdateParams {
                        status: Some(TaskStatus::Backlog),
                        ..Default::default()
                    },
                    "fixture".into(),
                )
                .unwrap();
            let prepared = workspace.prepare(&[&task.id]);
            if admitted {
                workspace
                    .runtime
                    .admit_task_for_workflow(&task.id, "worktree_setup")
                    .unwrap();
            }
            let params = match field {
                "description" => TaskUpdateParams {
                    description: Some("Operator changed the mandate.".into()),
                    ..Default::default()
                },
                "plan" => TaskUpdateParams {
                    plan: Some("Operator changed the plan.".into()),
                    ..Default::default()
                },
                _ => TaskUpdateParams {
                    context_files: Some(vec!["file:README.md".into()]),
                    ..Default::default()
                },
            };
            let edited = workspace
                .runtime
                .update_task_as_human(&task.id, params, "fixture".into())
                .unwrap();
            let mut input = apply_input(&workspace, &prepared);
            if field == "context" {
                input["results"][0]["tasks"][0]["context_files_before"] =
                    json!(edited.context_files);
            }
            let output = workspace.action("apply_task_pilot_results", input);
            assert_superseded(&workspace, &output, 1);
            assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), edited);
        }
    }
}

fn agent_before_echo_is_advisory_and_replays_use_the_prepared_snapshot() {
    let workspace = Workspace::new();
    workspace.commit_file("src/lib.rs", "fn fixture() {}\n", "add source");
    for (case, echo) in [
        ("reordered", Some(json!(["dir:src", "file:README.md"]))),
        ("normalized", Some(json!(["./README.md", "file:src"]))),
        ("different", Some(json!(["file:other.rs"]))),
        ("omitted", None),
    ] {
        let task = workspace.task(case);
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    context_files: Some(vec!["file:README.md".into(), "dir:src".into()]),
                    ..Default::default()
                },
                "fixture".into(),
            )
            .unwrap();
        let prepared = workspace.prepare(&[&task.id]);
        let mut input = apply_input(&workspace, &prepared);
        let assessment = &mut input["results"][0]["tasks"][0];
        if let Some(echo) = echo {
            assessment["context_files_before"] = echo;
        } else {
            assessment
                .as_object_mut()
                .unwrap()
                .remove("context_files_before");
        }
        let output = workspace.action("apply_task_pilot_results", input);
        assert_eq!(output["status"], "succeeded", "{case}: {output}");
        assert_eq!(output["applied_count"], 1, "{case}: {output}");
        assert_eq!(
            output["tasks"][0]["context_files_before"],
            prepared["tasks"][0]["context_files_before"]
        );
        let applied = workspace.runtime.get_task(&task.id).unwrap();
        assert_eq!(applied.context_files, ["file:README.md"]);
        let history = workspace.runtime.get_task_history(&task.id).unwrap();
        let mut replay = apply_input(&workspace, &prepared);
        replay["results"][0]["tasks"][0]["context_files_before"] = json!(["a different echo"]);
        let replayed = workspace.action("apply_task_pilot_results", replay);
        assert_eq!(replayed["status"], "succeeded", "{case}: {replayed}");
        assert_eq!(replayed["tasks"][0]["outcome"], "already_applied");
        assert_eq!(
            replayed["tasks"][0]["operation_id"],
            output["tasks"][0]["operation_id"]
        );
        assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), applied);
        assert_eq!(
            workspace.runtime.get_task_history(&task.id).unwrap(),
            history
        );
    }
}

fn promoting_ci_sweep_replay_settles_already_applied_without_writes() {
    let workspace = Workspace::new();
    let task = workspace.task("promoting CI-sweep replay");
    workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                tags: Some(vec![
                    "ci-failure-sweep".into(),
                    "ci-failure:fixture-key".into(),
                ]),
                ..Default::default()
            },
            "fixture".into(),
        )
        .unwrap();
    let prepared = workspace.prepare(&[&task.id]);
    let mut input = apply_input(&workspace, &prepared);
    input["ci_sweep_filing"] = json!({
        "task_id": task.id,
        "failure_key": "fixture-key",
        "tested_commit": workspace.object("HEAD"),
        "workflow": "ci", "job": "test", "step": "cargo test",
        "run_urls": ["https://github.com/example/repo/actions/runs/1"],
    });
    input["promotion_authorized"] = json!(true);
    let output = workspace.action("apply_task_pilot_results", input.clone());
    assert_eq!(output["status"], "succeeded", "{output}");
    assert_eq!(output["tasks"][0]["outcome"], "applied", "{output}");
    assert_eq!(output["ci_sweep_admission"][0]["decision"], "promote");
    let applied = workspace.runtime.get_task(&task.id).unwrap();
    assert_eq!(applied.status, TaskStatus::Backlog);
    assert_eq!(applied.context_files, ["file:README.md"]);
    let history = workspace.runtime.get_task_history(&task.id).unwrap();

    // The atomic promotion landed, but the step checkpoint was lost. The
    // engine retries the identical input against the now-backlog task.
    let replayed = workspace.action("apply_task_pilot_results", input.clone());
    assert_eq!(replayed["status"], "succeeded", "{replayed}");
    assert_eq!(replayed["tasks"][0]["outcome"], "already_applied");
    assert_eq!(replayed["applied_count"], 1);
    assert_eq!(replayed["unresolved_count"], 0);
    assert_eq!(replayed["repair_partitions"], json!([]));
    assert_eq!(replayed["ci_sweep_admission"], output["ci_sweep_admission"]);
    assert_eq!(
        replayed["tasks"][0]["operation_id"],
        output["tasks"][0]["operation_id"]
    );
    assert_eq!(
        workspace.action("pipeline_success_guard", json!({"result": replayed}))["succeeded"],
        true
    );
    assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), applied);
    assert_eq!(
        workspace.runtime.get_task_history(&task.id).unwrap(),
        history
    );

    // A different operation cannot borrow the committed promotion's receipt:
    // to it the task was promoted elsewhere, so it admits and writes nothing.
    input["results"][0]["tasks"][0]["assessment_rationale"] =
        json!("A new assessment of the failure.");
    let changed = workspace.action("apply_task_pilot_results", input);
    assert_eq!(changed["status"], "succeeded", "{changed}");
    assert_eq!(changed["applied_count"], 0, "{changed}");
    assert_eq!(changed["ci_sweep_admission"], json!([]), "{changed}");
    assert_eq!(changed["task_outcomes"][0]["reason"], "promoted_elsewhere");
    assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), applied);
    assert_eq!(
        workspace.runtime.get_task_history(&task.id).unwrap(),
        history
    );
}

fn superseded_sibling_survives_targeted_repair_and_a_valid_sibling_applies() {
    let workspace = Workspace::new();
    let tasks = ["retired", "repair", "valid"].map(|title| workspace.task(title));
    let prepared = workspace.prepare(
        &tasks
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>(),
    );
    workspace
        .runtime
        .update_task_as_human(
            &tasks[0].id,
            TaskUpdateParams {
                status: Some(TaskStatus::Rejected),
                ..Default::default()
            },
            "fixture".into(),
        )
        .unwrap();
    let mut input = apply_input(&workspace, &prepared);
    input["results"][0]["tasks"][1]["recommended_complexity"] = json!("invalid");
    let first = workspace.action("apply_task_pilot_results", input);
    assert_eq!(first["status"], "failed");
    assert_eq!(first["repair_count"], 1);
    assert_eq!(first["applied_count"], 1);
    let mut repair = apply_input(&workspace, &first["repair_prepared"]);
    repair["prior_applied_count"] = first["applied_count"].clone();
    repair["carried_task_outcomes"] = first["non_repairable_outcomes"].clone();
    let output = workspace.action("apply_task_pilot_results", repair);
    assert_superseded(&workspace, &output, 1);
    assert_eq!(output["applied_count"], 2);
}

fn local_auto_ship_and_readiness_wait_for_an_active_pilot_checkpoint() {
    let workspace = Workspace::new();
    let task = workspace.task("ready to deliver");
    workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                context_files: Some(vec!["file:README.md".into()]),
                complexity: Some(TaskComplexity::Low),
                ..Default::default()
            },
            "fixture".into(),
        )
        .unwrap();
    let holder = workspace.hold(&[&task.id]);
    if let Ok(binary) = std::env::var("ORBIT_TEST_CANDIDATE") {
        let output = std::process::Command::new(binary)
            .args(["run", "ship", &task.id, "--mode", "local"])
            .current_dir(&workspace.repo)
            .env("HOME", workspace.root.path().join("home"))
            .output()
            .unwrap();
        let terminal = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.status.success(), "{terminal}");
        assert!(
            terminal.contains(&task.id) && terminal.contains("active_pilot_preparation"),
            "{terminal}"
        );
    }
    let held_ship = workspace
        .runtime
        .submit_ship_run(orbit_core::ShipRunRequest {
            mode: orbit_types::workflow::ShipMode::Local,
            base_branch: Some("main"),
            task_ids: std::slice::from_ref(&task.id),
            completion: orbit_types::workflow::CompletionPolicy::Review,
            allowed_crews: &[],
            actor: None,
            claim_token: None,
            trigger: orbit_types::workflow::JobRunTrigger::cli(),
            strict_containment: false,
            review: None,
            operator_override: true,
        })
        .expect_err("named ship must report the active pilot hold");
    assert!(held_ship.to_string().contains(&task.id), "{held_ship}");
    assert!(held_ship.to_string().contains("task-pilot"), "{held_ship}");
    for input in [json!({}), json!({"task_ids": [task.id]})] {
        let output = workspace.action("list_backlog_tasks", input);
        assert_eq!(output["task_ids"], json!([]), "{output}");
        assert_eq!(
            output["excluded"][0]["reason"], "active_pilot_preparation",
            "{output}"
        );
    }
    // Exercise the shipped workflow after admission, including a race where
    // the pilot appears between submission and backlog listing.
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let activities = workspace
        .root
        .path()
        .join("home/.orbit/resources/activities");
    for name in ["list_backlog_tasks", "validate_bundles", "invoke_and_wait"] {
        std::fs::copy(
            assets.join(format!("activities/{name}.yaml")),
            activities.join(format!("{name}.yaml")),
        )
        .unwrap();
    }
    let job = assets.join("jobs/task_auto_pipeline.yaml");
    let held = workspace
        .runtime
        .run_job_v2_from_yaml(&job, json!({"task_ids": [task.id], "mode": "local"}))
        .expect_err("held named run must fail");
    assert!(
        held.to_string().contains(&task.id)
            && held.to_string().contains("active_pilot_preparation"),
        "{held}"
    );
    let runs = workspace.jobs.list_job_runs("task_auto_pipeline").unwrap();
    assert_eq!(runs.len(), 1);
    let audit = workspace.runtime.show_job_run(&runs[0].run_id).unwrap();
    assert_eq!(audit.state, JobRunState::Failed);
    assert!(
        format!("{audit:?}").contains("active_pilot_preparation"),
        "{audit:?}"
    );
    let empty = workspace
        .runtime
        .run_job_v2_from_yaml(&job, json!({"mode": "local"}))
        .unwrap();
    assert!(empty.success, "empty polling succeeds: {empty:?}");
    let wave = workspace.action(
        "classify_workspace_auto_tasks",
        json!({"max_active_leaf_runs": 1}),
    );
    assert_eq!(wave["loose_task_ids"], json!([]), "{wave}");
    let ready = workspace
        .runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    assert_eq!(
        ready["tasks"][0]["reason"], "active_pilot_preparation",
        "{ready}"
    );
    workspace
        .jobs
        .finalize_job_run(&holder, JobRunState::Success, Utc::now(), None)
        .unwrap();
    assert_eq!(
        workspace.action("list_backlog_tasks", json!({}))["task_ids"],
        json!([task.id])
    );
}

#[test]
#[ignore = "entry point for the pilot overlap fixture worker"]
fn pilot_overlap_gate_worker() {
    let global = std::env::var_os("ORBIT_TEST_PILOT_GLOBAL").unwrap();
    let workspace = std::env::var_os("ORBIT_TEST_PILOT_WORKSPACE").unwrap();
    let run = std::env::var("ORBIT_TEST_PILOT_RUN").unwrap();
    let runtime = runtime_at(Path::new(&global), Path::new(&workspace));
    install_pilot_overlap_worker(&runtime);
    runtime.execute_pipeline_run_worker(&run).unwrap();
}

fn install_pilot_overlap_worker(runtime: &OrbitRuntime) {
    let worker = "task_pilot::races::pilot_overlap_gate_worker";
    orbit_common::test_env::assert_child_test_exists(worker);
    orbit_core::test_support::install_substitute_pipeline_worker([
        "env".into(),
        format!(
            "ORBIT_TEST_PILOT_GLOBAL={}",
            runtime.global_root().display()
        ),
        format!(
            "ORBIT_TEST_PILOT_WORKSPACE={}",
            runtime.paths().orbit_dir.display()
        ),
        "ORBIT_TEST_PILOT_RUN={run_id}".into(),
        std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        "--exact".into(),
        worker.into(),
        "--ignored".into(),
        "--nocapture".into(),
    ]);
}

fn mixed_named_shipment_dispatches_eligible_and_audits_hold() {
    let workspace = Workspace::new();
    let held = workspace.task("pilot held");
    let eligible = workspace.task("eligible sibling");
    for task in [&held, &eligible] {
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    status: Some(TaskStatus::Backlog),
                    context_files: Some(vec!["file:README.md".into()]),
                    complexity: Some(TaskComplexity::Low),
                    ..Default::default()
                },
                "fixture".into(),
            )
            .unwrap();
    }
    let holder = workspace.hold(&[&held.id]);
    let resources = workspace.runtime.global_root().join("resources");
    std::fs::create_dir_all(resources.join("jobs")).unwrap();
    for name in ["list_backlog_tasks", "validate_bundles", "invoke_and_wait"] {
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("assets/activities/{name}.yaml")),
            resources.join(format!("activities/{name}.yaml")),
        )
        .unwrap();
    }
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/jobs/task_auto_pipeline.yaml"),
        resources.join("jobs/task_auto_pipeline.yaml"),
    )
    .unwrap();
    std::fs::write(resources.join("jobs/task_gate_pipeline.yaml"), "schemaVersion: 2\nkind: Job\nmetadata:\n  name: task_gate_pipeline\nspec:\n  state: enabled\n  steps: []\n").unwrap();
    install_pilot_overlap_worker(&workspace.runtime);
    let request = |ids: &[String]| {
        workspace
            .runtime
            .submit_ship_run(orbit_core::ShipRunRequest {
                mode: orbit_types::workflow::ShipMode::Local,
                base_branch: Some("main"),
                task_ids: ids,
                operator_override: true,
                ..Default::default()
            })
            .unwrap()
    };
    let mixed = request(&[held.id.clone(), eligible.id.clone()]);
    workspace
        .runtime
        .run_tool(
            "orbit.pipeline.wait",
            json!({"run_ids": [mixed.run_id], "timeout_seconds": 60}),
        )
        .unwrap();
    let audit = workspace.runtime.show_job_run(&mixed.run_id).unwrap();
    assert_eq!(audit.state, JobRunState::Failed, "{audit:?}");
    assert!(
        format!("{audit:?}").contains("active_pilot_preparation"),
        "{audit:?}"
    );
    let children = workspace.jobs.list_job_runs("task_gate_pipeline").unwrap();
    assert_eq!(children.len(), 1, "{children:?}");
    assert_eq!(
        children[0].input.as_ref().unwrap()["task_ids"],
        json!([eligible.id])
    );
    workspace
        .jobs
        .finalize_job_run(&holder, JobRunState::Success, Utc::now(), None)
        .unwrap();
    let retry = request(std::slice::from_ref(&held.id));
    workspace
        .runtime
        .run_tool(
            "orbit.pipeline.wait",
            json!({"run_ids": [retry.run_id], "timeout_seconds": 60}),
        )
        .unwrap();
    let children = workspace.jobs.list_job_runs("task_gate_pipeline").unwrap();
    assert_eq!(children.len(), 2, "{children:?}");
    assert_eq!(
        children
            .iter()
            .filter(|child| child.input.as_ref().unwrap()["task_ids"] == json!([held.id]))
            .count(),
        1
    );
    assert_eq!(
        children
            .iter()
            .filter(|child| child.input.as_ref().unwrap()["task_ids"] == json!([eligible.id]))
            .count(),
        1
    );
}

// Related boundary scenarios share the isolated table fixture. The row that
// installs the process-wide pilot worker substitution has its own table below,
// so no other row can inherit it whatever the row order.
crate::scenario_table!(
    admitted_and_retired_targets_settle_as_superseded_without_pilot_writes_scenarios,
    admitted_and_retired_targets_settle_as_superseded_without_pilot_writes,
    human_material_edits_supersede_preparation_without_writes,
    agent_before_echo_is_advisory_and_replays_use_the_prepared_snapshot,
    promoting_ci_sweep_replay_settles_already_applied_without_writes,
    superseded_sibling_survives_targeted_repair_and_a_valid_sibling_applies,
    local_auto_ship_and_readiness_wait_for_an_active_pilot_checkpoint
);

crate::scenario_table!(
    pilot_overlap_worker_scenarios,
    mixed_named_shipment_dispatches_eligible_and_audits_hold
);
