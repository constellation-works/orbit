//! A malformed apply input is refused whole, before any task is written, with
//! the refusal naming the offending field; a malformed partition result fails
//! only its partition. Fixtures drive the real prepare and apply actions in
//! isolated children.

use orbit_core::application::task::TaskAddParams;
use orbit_core::{Task, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use serde_json::{Value, json};

use super::Workspace;

fn proposed(workspace: &Workspace, title: &str) -> Task {
    workspace
        .runtime
        .add_task(TaskAddParams {
            title: title.into(),
            description: format!("Prepare {title}."),
            acceptance_criteria: vec!["Selectors identify the implementation scope.".into()],
            plan: "Inspect README.md.".into(),
            status: Some(TaskStatus::Proposed),
            ..Default::default()
        })
        .unwrap()
}

fn assessment(task_id: &str) -> Value {
    json!({
        "task_id": task_id, "context_files_after": ["file:README.md"],
        "disposition": "selectors", "recommended_crew": "fixture",
        "recommended_complexity": "low", "confidence": "high",
        "assessment_rationale": "README.md contains the affected material.",
        "validation_approach": "Inspect the persisted scope.",
        "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
        "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
        "duplicate_of": null, "already_landed": null,
    })
}

fn apply_input(workspace: &Workspace, prepared: &Value, task_id: &str) -> Value {
    json!({
        "workspace_path": workspace.repo, "prepared": prepared,
        "results": [{"partition_index": 0, "task_ids": [task_id], "tasks": [assessment(task_id)]}],
    })
}

fn apply(workspace: &Workspace, input: &Value) -> Result<Value, String> {
    // The fixture pilot read its checkout: results that do not carry an
    // observed-revision proof get the real one, as in `Workspace::action`.
    let input = workspace.with_inspection_proof(input.clone());
    workspace
        .runtime
        .run_deterministic(
            "apply_task_pilot_results",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .map_err(|error| error.to_string())
}

fn malformed_apply_input_is_refused_before_any_task_is_written() {
    #[cfg(unix)]
    explicit_status_is_refused_before_assessment();
    let workspace = Workspace::new();
    let task = proposed(&workspace, "malformed");
    let other = proposed(&workspace, "sibling");
    let id = task.id.as_str();
    let prepared = workspace.prepare(&[id]);
    let pair = workspace.prepare(&[id, &other.id]);
    let root = workspace.repo.canonicalize().unwrap();

    type Edit = Box<dyn Fn(&mut Value)>;
    let cases: Vec<(&str, Edit, String)> = vec![
        (
            "prepared absent",
            Box::new(|input| {
                input.as_object_mut().unwrap().remove("prepared");
            }),
            "`prepared` must be an object".into(),
        ),
        (
            "prepared scalar",
            Box::new(|input| input["prepared"] = json!("prepared")),
            "`prepared` must be an object".into(),
        ),
        (
            "workspace absent",
            Box::new(|input| {
                input["prepared"]
                    .as_object_mut()
                    .unwrap()
                    .remove("workspace_path");
            }),
            "prepared.workspace_path must be a string".into(),
        ),
        (
            "another workspace",
            Box::new(|input| input["prepared"]["workspace_path"] = json!("/elsewhere")),
            format!(
                "prepared workspace /elsewhere does not match active workspace {}",
                root.display()
            ),
        ),
        (
            "mode not a string",
            Box::new(|input| input["prepared"]["mode"] = json!(5)),
            "prepared.mode must be a string".into(),
        ),
        (
            "partitions not an array",
            Box::new(|input| input["prepared"]["partitions"] = json!({})),
            "prepared.partitions must be an array".into(),
        ),
        (
            "tasks null",
            Box::new(|input| input["prepared"]["tasks"] = Value::Null),
            "prepared.tasks must be an array".into(),
        ),
        (
            "results not an array",
            Box::new(|input| input["results"] = json!({})),
            "`results` must be an array".into(),
        ),
        (
            "carried outcomes null",
            Box::new(|input| input["carried_task_outcomes"] = Value::Null),
            "carried_task_outcomes must be an array".into(),
        ),
        (
            "blank task id",
            Box::new(|input| input["prepared"]["tasks"][0]["task_id"] = json!("  ")),
            "`task_id` must be a non-empty string".into(),
        ),
        (
            "task snapshot not an object",
            Box::new(|input| input["prepared"]["tasks"][0] = json!(["task"])),
            "`task_id` must be a non-empty string".into(),
        ),
        (
            "non-string selector",
            Box::new(|input| input["prepared"]["tasks"][0]["context_files_before"] = json!([1])),
            "`context_files_before` must contain only strings".into(),
        ),
        (
            "status absent",
            Box::new(|input| {
                input["prepared"]["tasks"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("status");
            }),
            "prepared task is missing status".into(),
        ),
        (
            "status unknown",
            Box::new(|input| input["prepared"]["tasks"][0]["status"] = json!("nonsense")),
            "prepared task status is invalid: unknown variant `nonsense`".into(),
        ),
        (
            "blank title",
            Box::new(|input| input["prepared"]["tasks"][0]["title"] = json!("")),
            "`title` must be a non-empty string".into(),
        ),
        (
            "tags not an array",
            Box::new(|input| input["prepared"]["tasks"][0]["tags"] = json!("tag")),
            "`tags` must be an array".into(),
        ),
        (
            "complexity unknown",
            Box::new(|input| input["prepared"]["tasks"][0]["complexity"] = json!("huge")),
            "prepared task complexity is invalid: unknown variant `huge`".into(),
        ),
        (
            "tool warnings not an array",
            Box::new(|input| input["prepared"]["tasks"][0]["validation_tool_warnings"] = json!(3)),
            "`validation_tool_warnings` must be an array".into(),
        ),
        (
            "components not an object",
            Box::new(|input| input["prepared"]["tasks"][0]["material_components"] = json!([])),
            "prepared task material_components must be an object of component digests".into(),
        ),
        (
            "component digest not a string",
            Box::new(|input| {
                input["prepared"]["tasks"][0]["material_components"] =
                    json!({"title": "digest", "plan": 1})
            }),
            "prepared task material_components.plan must be a string digest".into(),
        ),
        (
            "negative history length",
            Box::new(|input| input["prepared"]["tasks"][0]["history_len"] = json!(-1)),
            "invalid history_len: invalid value: integer `-1`".into(),
        ),
        (
            "creation identity not a string",
            Box::new(|input| input["prepared"]["tasks"][0]["context_creation_identity"] = json!(7)),
            "prepared task context_creation_identity must be a string or null".into(),
        ),
        (
            "duplicate snapshots",
            Box::new(|input| {
                let snapshot = input["prepared"]["tasks"][0].clone();
                input["prepared"]["tasks"]
                    .as_array_mut()
                    .unwrap()
                    .push(snapshot);
            }),
            "prepared.tasks contains duplicate task snapshots".into(),
        ),
        (
            "promotion authority not a boolean",
            Box::new(|input| input["promotion_authorized"] = Value::Null),
            "promotion_authorized must be a boolean".into(),
        ),
        (
            "drain without authority",
            Box::new(|input| input["drain_promotion"] = json!({"run_id": "jrun-x"})),
            "drain_promotion requires promotion_authorized".into(),
        ),
        (
            "two authorities",
            Box::new(|input| {
                input["ci_sweep_filing"] = json!({"task_id": "ORB-1"});
                input["drain_promotion"] = json!({"run_id": "jrun-x"});
            }),
            "ci_sweep_filing and drain_promotion are separate authorities; supply one".into(),
        ),
        (
            "filing for another task",
            Box::new(|input| input["ci_sweep_filing"] = json!({"task_id": "ORB-other"})),
            format!("CI-sweep filing names task ORB-other, expected {id}"),
        ),
        (
            "partition index invalid",
            Box::new(|input| input["prepared"]["partitions"][0]["partition_index"] = json!("0")),
            "prepared.partitions[0].partition_index is invalid".into(),
        ),
        (
            "partition not an object",
            Box::new(|input| input["prepared"]["partitions"][0] = json!([0, ["ORB-1"]])),
            "prepared.partitions[0].partition_index is invalid".into(),
        ),
        (
            "partition repeats a task",
            Box::new(|input| {
                let id = input["prepared"]["tasks"][0]["task_id"].clone();
                input["prepared"]["partitions"][0]["task_ids"] = json!([id, id]);
            }),
            format!("task {id} appears in more than one prepared partition"),
        ),
        (
            "partition names an unprepared task",
            Box::new(|input| input["prepared"]["partitions"][0]["task_ids"] = json!(["ORB-404"])),
            "task ORB-404 was not in prepared.tasks".into(),
        ),
        (
            "partitions miss a task",
            Box::new(|input| input["prepared"]["partitions"] = json!([])),
            "prepared partitions did not cover every prepared task".into(),
        ),
    ];
    for (label, edit, expected) in cases {
        let mut input = apply_input(&workspace, &prepared, id);
        edit(&mut input);
        let refused = apply(&workspace, &input).expect_err(label);
        assert!(
            refused.contains(&expected),
            "{label}: expected `{expected}` in `{refused}`"
        );
        assert!(
            workspace
                .runtime
                .get_task(id)
                .unwrap()
                .context_files
                .is_empty(),
            "{label}: a refused apply must not write the task"
        );
    }

    let mut input = apply_input(&workspace, &pair, id);
    input["ci_sweep_filing"] = json!({"task_id": id});
    let refused = apply(&workspace, &input).unwrap_err();
    assert!(
        refused.contains("CI-sweep admission requires exactly one prepared task"),
        "{refused}"
    );

    // The unedited input still applies, so each refusal above is the edit's.
    let applied = apply(&workspace, &apply_input(&workspace, &prepared, id)).unwrap();
    assert_eq!(applied["status"], "succeeded", "{applied}");
}

fn malformed_partition_result_fails_only_its_partition() {
    let workspace = Workspace::new();
    // A refusal raised by a shared field reader keeps its action prefix.
    let refusal = |message: &str| {
        format!("deterministic action `apply_task_pilot_results` failed: {message}")
    };
    type PilotResult = Box<dyn Fn(&str) -> Value>;
    let cases: Vec<(&str, PilotResult, String)> = vec![
        (
            "scalar result",
            Box::new(|_| json!("done")),
            "partition 0 failed or returned no structured result".into(),
        ),
        (
            "non-string task id",
            Box::new(|id| json!({"task_ids": [id, 1], "tasks": [assessment(id)]})),
            refusal("`task_ids` must contain only strings"),
        ),
        (
            "task ids absent",
            Box::new(|id| json!({"tasks": [assessment(id)]})),
            refusal("`task_ids` must be an array"),
        ),
        (
            "tasks not an array",
            Box::new(|id| json!({"task_ids": [id], "tasks": {"task": assessment(id)}})),
            "partition 0.tasks must be an array".into(),
        ),
        (
            "assessment without an id",
            Box::new(|id| {
                let mut task = assessment(id);
                task.as_object_mut().unwrap().remove("task_id");
                json!({"task_ids": [id], "tasks": [task]})
            }),
            refusal("`task_id` must be a non-empty string"),
        ),
    ];
    for (label, result, expected) in cases {
        let task = proposed(&workspace, label);
        let prepared = workspace.prepare(&[&task.id]);
        let input = json!({
            "workspace_path": workspace.repo, "prepared": prepared,
            "results": [result(&task.id)],
        });
        let applied = apply(&workspace, &input).unwrap();
        assert_eq!(applied["status"], "failed", "{label}: {applied}");
        let decision = &applied["partition_decisions"][0];
        assert_eq!(decision["outcome"], "failed", "{label}: {decision}");
        assert_eq!(decision["error"], expected, "{label}: {decision}");
        assert_eq!(applied["repair_count"], 1, "{label}: {applied}");
        assert_eq!(
            applied["error"],
            format!(
                "task-pilot apply unresolved: partition 0, task {}: invalid: {expected}; 0 applied, 1 unresolved",
                task.id
            ),
            "{label}"
        );
    }
}

// Run the shipped job, with only the provider's answer scripted. Counting the
// launcher proves refusal happens before assessment, rather than at apply.
#[cfg(unix)]
fn explicit_status_is_refused_before_assessment() {
    use orbit_core::application::task::TaskUpdateParams;
    use orbit_types::resource::ExecutorResource;
    use orbit_types::workflow::{ExecutorDef, ExecutorSandboxKind};
    use std::os::unix::fs::PermissionsExt;

    let workspace = Workspace::new();
    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let calls = workspace.root.path().join("provider-calls");
    let response = workspace.root.path().join("response.json");
    let provider = workspace.root.path().join("codex");
    std::fs::write(&calls, "").unwrap();
    std::fs::write(
        &provider,
        format!(
            "#!/bin/sh\n/bin/cat > /dev/null\nprintf 'call\\n' >> '{}'\n/bin/cat '{}'\n",
            calls.display(),
            response.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut executor: ExecutorResource = serde_yaml::from_str(
        &std::fs::read_to_string(assets.join("executors/codex.yaml")).unwrap(),
    )
    .unwrap();
    executor.spec.command = Some(provider.display().to_string());
    // This fixture observes dispatch and state, independently of OS confinement.
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
    let allowed = proposed(&workspace, "allowed sibling");
    let target = proposed(&workspace, "unsupported target");
    let reservations = workspace
        .runtime
        .paths()
        .state_dir
        .join("task-pilot-reservations.json");
    // The third row models a successful command preflight followed by a move
    // before the authoritative prepare starts. No scheduling or timing race.
    for (mixed, moved) in [(false, false), (true, false), (true, true)] {
        if moved {
            workspace
                .runtime
                .force_update_task_with_identity(
                    &target.id,
                    TaskUpdateParams {
                        status: Some(TaskStatus::Backlog),
                        ..Default::default()
                    },
                    None,
                    None,
                )
                .unwrap();
            assert_eq!(
                workspace.runtime.get_task(&target.id).unwrap().status,
                TaskStatus::Backlog
            );
        }
        workspace
            .runtime
            .force_update_task_with_identity(
                &target.id,
                TaskUpdateParams {
                    status: Some(TaskStatus::Blocked),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
        let ids = if mixed {
            vec![allowed.id.as_str(), target.id.as_str()]
        } else {
            vec![target.id.as_str()]
        };
        let before = [&allowed.id, &target.id].map(|id| {
            json!({
                "task": workspace.runtime.get_task(id).unwrap(),
                "history": workspace.runtime.get_task_history(id).unwrap(),
            })
        });
        let reservation_before = std::fs::read(&reservations).ok();
        let outcome = workspace
            .runtime
            .run_job_v2_from_yaml(
                &assets.join("jobs/task_pilot_pipeline.yaml"),
                json!({"task_ids": ids, "workspace_path": workspace.repo, "base_branch": "main"}),
            )
            .expect_err("unsupported explicit selections must refuse preparation");
        let evidence = outcome.to_string();
        for expected in [target.id.as_str(), "blocked", "proposed", "backlog"] {
            assert!(
                evidence.contains(expected),
                "refusal must name task/status and eligibility: {evidence}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(&calls).unwrap(),
            "",
            "refused before provider"
        );
        assert_eq!(std::fs::read(&reservations).ok(), reservation_before);
        for (id, before) in [&allowed.id, &target.id].into_iter().zip(before) {
            assert_eq!(
                json!({"task": workspace.runtime.get_task(id).unwrap(),
                "history": workspace.runtime.get_task_history(id).unwrap()}),
                before
            );
        }
    }
    let rows = workspace
        .runtime
        .v2_audit_store()
        .unwrap()
        .list_v2_audit_events(&orbit_store::contracts::V2AuditEventFilter {
            workspace_id: workspace.runtime.workspace_id().unwrap(),
            limit: Some(1000),
            ..Default::default()
        })
        .unwrap();
    let events = rows
        .iter()
        .map(|row| serde_json::from_str::<Value>(&row.payload_json).unwrap())
        .collect::<Vec<_>>();
    let finished = events
        .iter()
        .filter(|event| event["body_kind"] == "run_finished")
        .collect::<Vec<_>>();
    assert_eq!(
        finished.len(),
        3,
        "each refusal keeps its terminal audit record"
    );
    for event in finished {
        assert_eq!(event["outcome"], "error", "{event}");
        assert!(
            event["error_message"]
                .as_str()
                .unwrap()
                .contains("only proposed and backlog"),
            "{event}"
        );
    }
    // Both eligible states still traverse the real agent and apply steps.
    for status in [TaskStatus::Proposed, TaskStatus::Backlog] {
        let task = proposed(&workspace, &format!("eligible {status}"));
        if status == TaskStatus::Backlog {
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
        // The scripted pilot read its checkout: its static answer carries
        // the observed-revision proof of the revision prepare pins (the
        // fixture's `main` HEAD, which nothing moves during the run).
        let revision = workspace.git(&["rev-parse", "HEAD"]).trim().to_string();
        let tree = workspace
            .git(&["rev-parse", "HEAD^{tree}"])
            .trim()
            .to_string();
        std::fs::write(&response, json!({"schemaVersion": 1, "status": "success", "error": null,
            "result": {"partition_index": 0, "observed_revision": revision, "observed_tree": tree,
                "task_ids": [task.id], "tasks": [assessment(&task.id)]}
        }).to_string()).unwrap();
        let outcome = workspace.runtime.run_job_v2_from_yaml(
            &assets.join("jobs/task_pilot_pipeline.yaml"),
            json!({"task_ids": [task.id], "workspace_path": workspace.repo, "base_branch": "main"}),
        ).unwrap();
        assert!(outcome.success, "{status}: {outcome:?}");
        assert_eq!(outcome.pipeline["apply"]["applied_count"], 1);
        let after = workspace.runtime.get_task(&task.id).unwrap();
        assert_eq!(after.status, status);
        assert_eq!(after.context_files, ["file:README.md"]);
    }
    assert_eq!(std::fs::read_to_string(calls).unwrap().lines().count(), 2);
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    malformed_apply_input_is_refused_before_any_task_is_written_scenarios,
    malformed_apply_input_is_refused_before_any_task_is_written,
    malformed_partition_result_fails_only_its_partition
);
