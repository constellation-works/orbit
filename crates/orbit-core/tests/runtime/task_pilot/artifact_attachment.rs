//! Screenshot and task-artifact criteria are feasible in the implement lane.
//!
//! A ui task whose criteria ask for screenshots attached as task artifacts
//! must not be reported as missing `orbit.task.artifact.put`. Owner mode
//! calls that tool; claimed mode attaches declared files through the
//! handoff. A criterion that names a tool the lane does not grant still
//! warns, and a warning that names both stays.

use std::path::Path;

use orbit_core::application::task::TaskAddParams;
use orbit_core::{Task, TaskComplexity, TaskStatus};
use serde_json::{Value, json};

use super::Workspace;

const INCIDENT: &str = "Acceptance criterion 4 requires screenshots attached as task artifacts, but the supplied tool allowlist has no orbit.task.artifact.put and required_tools is empty";
const OPERATOR: &str = "The operator should review the proposed approach.";
const UNGRANTED: &str = "acceptance criterion requires `orbit.pipeline.invoke`, which the `agent_implement` allowlist does not grant and this task's `required_tools` does not declare";
const MIXED: &str = "Acceptance criterion 1 requires screenshots attached as task artifacts, and acceptance criterion 2 requires `orbit.pipeline.invoke`, but the supplied tool allowlist does not grant that call and required_tools is empty";

fn install_real_implement_activity(workspace: &Workspace) {
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/activities/agent_implement.yaml");
    let dest = workspace
        .runtime
        .global_root()
        .join("resources/activities/agent_implement.yaml");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::copy(source, &dest).unwrap();
}

fn ui_task(workspace: &Workspace, criteria: Vec<String>) -> Task {
    workspace
        .runtime
        .add_task(TaskAddParams {
            title: "Show drain approval findings".into(),
            description: "The dashboard approval hold should name each pilot warning.".into(),
            acceptance_criteria: criteria,
            plan: "Render the finding under the approved count.".into(),
            status: Some(TaskStatus::Proposed),
            complexity: TaskComplexity::Low,
            context_files: vec!["file:README.md".into()],
            tags: vec!["ui".into()],
            ..Default::default()
        })
        .unwrap()
}

fn apply(workspace: &Workspace, task: &Task, utility_warnings: &[&str]) -> Value {
    let prepared = workspace.prepare(&[&task.id]);
    let lane = &prepared["implementation_lane"];
    assert_eq!(lane["activity"], "agent_implement", "{lane}");
    let tools = lane["tools"].as_array().expect("lane tools");
    assert!(
        tools.iter().any(|tool| tool == "orbit.task.artifact.put"),
        "owner attachment must be callable: {lane}"
    );
    assert!(
        !tools.iter().any(|tool| tool == "orbit.pipeline.invoke"),
        "the implement disallow list must still exclude orbit.pipeline.invoke: {lane}"
    );
    assert_eq!(
        lane["artifact_attachment"]["owner"],
        "orbit.task.artifact.put"
    );
    assert_eq!(lane["artifact_attachment"]["owner_granted"], true);
    assert_eq!(
        lane["artifact_attachment"]["claimed"],
        "task_artifacts handoff"
    );
    let assessment = json!({
        "task_id": task.id,
        "context_files_before": task.context_files,
        "context_files_after": ["file:README.md"],
        "disposition": "selectors",
        "recommended_crew": "fixture",
        "recommended_complexity": "low",
        "confidence": "high",
        "assessment_rationale": "The drain card is the affected surface.",
        "validation_approach": "Exercise the dashboard approval hold.",
        "evidence_gaps": [],
        "reassessment_triggers": [],
        "blocked_by": [],
        "adr_conflicts": [],
        "utility_warnings": utility_warnings,
        "surface_warnings": [],
        "duplicate_of": null,
        "already_landed": null,
        "user_facing_ui": true,
    });
    let applied = workspace.action(
        "apply_task_pilot_results",
        json!({
            "workspace_path": workspace.repo,
            "prepared": prepared,
            "results": [{"partition_index": 0, "task_ids": [task.id], "tasks": [assessment]}],
        }),
    );
    assert_eq!(applied["status"], "succeeded", "{applied}");
    let comments = workspace.runtime.get_task_comments(&task.id).unwrap();
    serde_json::from_str(comments.last().unwrap().receipt().unwrap().body).unwrap()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("expected an array: {value}"))
        .iter()
        .map(|item| {
            item.as_str()
                .unwrap_or_else(|| panic!("expected a string finding: {item}"))
                .to_string()
        })
        .collect()
}

fn mentions_artifact_tool(warnings: &[String]) -> bool {
    warnings.iter().any(|warning| {
        let lowered = warning.to_ascii_lowercase();
        lowered.contains("orbit.task.artifact.put") || lowered.contains("screenshot")
    })
}

fn screenshot_attachment_is_feasible_and_an_ungranted_tool_still_warns() {
    let screenshots_only = Workspace::new();
    install_real_implement_activity(&screenshots_only);
    let screenshots = ui_task(
        &screenshots_only,
        vec!["Attach screenshots of the changed dashboard view as task artifacts.".into()],
    );
    let audit = apply(&screenshots_only, &screenshots, &[INCIDENT, OPERATOR]);
    assert_eq!(
        strings(&audit["assessment"]["utility_warnings"]),
        vec![OPERATOR.to_string()],
        "a screenshot criterion must drop the false attachment warning and keep an unrelated one: {audit}"
    );
    let validation = strings(&audit["assessment"]["validation_tool_warnings"]);
    assert!(
        validation.is_empty(),
        "screenshot prose that names no tool is not an artifact-tool warning: {validation:?}"
    );

    let outside = Workspace::new();
    install_real_implement_activity(&outside);
    let task = ui_task(
        &outside,
        vec![
            "Attach screenshots of the changed dashboard view as task artifacts.".into(),
            "Confirm the repair by running `orbit.pipeline.invoke`.".into(),
        ],
    );
    let audit = apply(&outside, &task, &[INCIDENT, OPERATOR, UNGRANTED, MIXED]);
    assert_eq!(
        strings(&audit["assessment"]["utility_warnings"]),
        vec![
            OPERATOR.to_string(),
            UNGRANTED.to_string(),
            MIXED.to_string()
        ],
        "the ungranted-tool warning and a mixed warning stay, and the false attachment warning does not: {audit}"
    );
    let validation = strings(&audit["assessment"]["validation_tool_warnings"]);
    assert!(
        validation
            .iter()
            .any(|warning| warning.contains("orbit.pipeline.invoke")),
        "a tool outside the implement allowlist still warns: {validation:?}"
    );
    assert!(
        !mentions_artifact_tool(&validation),
        "the deterministic check must not warn about screenshot attachment: {validation:?}"
    );
}

crate::scenario_table!(
    screenshot_attachment_is_feasible_and_an_ungranted_tool_still_warns_scenarios,
    screenshot_attachment_is_feasible_and_an_ungranted_tool_still_warns,
);
