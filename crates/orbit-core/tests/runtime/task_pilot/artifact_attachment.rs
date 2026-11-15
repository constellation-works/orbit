//! Screenshot and task-artifact criteria are feasible in the implement lane.
//!
//! A ui task whose criteria ask for screenshots attached as task artifacts
//! must not be reported as missing `orbit.task.artifact.put`. Owner mode
//! calls that tool; claimed mode attaches declared files through the
//! handoff. A criterion that names a tool the lane does not grant still
//! warns, and a warning that names both stays. A warning that says the
//! screenshot cannot be captured or rendered describes the view, not the
//! attachment, so it stays and holds the drain's approval.

use std::path::Path;

use chrono::Utc;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{Task, TaskComplexity, TaskStatus};
use orbit_types::workflow::{ChildDispatch, PipelineState};
use serde_json::{Value, json};

use super::Workspace;

const INCIDENT: &str = "Acceptance criterion 4 requires screenshots attached as task artifacts, but the supplied tool allowlist has no orbit.task.artifact.put and required_tools is empty";
const OPERATOR: &str = "The operator should review the proposed approach.";
const UNGRANTED: &str = "acceptance criterion requires `orbit.pipeline.invoke`, which the `agent_implement` allowlist does not grant and this task's `required_tools` does not declare";
const MIXED: &str = "Acceptance criterion 1 requires screenshots attached as task artifacts, and acceptance criterion 2 requires `orbit.pipeline.invoke`, but the supplied tool allowlist does not grant that call and required_tools is empty";
const CAPTURE: &str = "Criterion 3 needs a screenshot artifact of the dashboard, but the implement lane has no headless browser, so capturing it is infeasible";
const RENDER: &str = "Criterion 2 needs a task artifact screenshot of the drain card, but the dashboard view cannot render in this lane, so the artifact is infeasible";

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

/// A running auto drain that started with `approve_proposed` and dispatched a
/// running pilot, so the pilot's apply carries the drain's verified authority.
fn drain_and_pilot(workspace: &Workspace) -> (String, String) {
    let drain = workspace
        .jobs
        .insert_job_run(
            "workspace_auto_pipeline",
            1,
            Utc::now(),
            Some(json!({"approve_proposed": true})),
            None,
        )
        .unwrap();
    workspace
        .jobs
        .mark_job_run_running(&drain.run_id, Utc::now(), std::process::id())
        .unwrap();
    let pilot = workspace.live_run();
    let mut state = PipelineState::new(drain.run_id.clone(), drain.job_id, json!({}));
    state.record_child_dispatch(ChildDispatch::submitted(
        pilot.clone(),
        "task_pilot_pipeline".into(),
        "invoke_detached".into(),
        false,
        false,
        Utc::now(),
    ));
    workspace
        .runtime
        .write_run_state(&drain.run_id, &state)
        .unwrap();
    (drain.run_id, pilot)
}

/// Apply `utility_warnings` for `task` under drain authority, as the drain's
/// pilot child does. Returns the stored audit and the drain's approval
/// decision for the task.
fn apply(workspace: &Workspace, task: &Task, utility_warnings: &[&str]) -> (Value, Value) {
    let prepared = workspace.prepare(&[&task.id]);
    let lane = &prepared["implementation_lane"];
    assert_eq!(lane["activity"], "agent_implement", "{lane}");
    let tools = lane["tools"].as_array().expect("lane tools");
    let names = tools
        .iter()
        .filter_map(|tool| tool.as_str().map(str::to_string))
        .collect::<Vec<_>>();
    assert!(
        orbit_types::workflow::tool_allowed("orbit.task.artifact:put", &names),
        "owner attachment must be callable: {lane}"
    );
    assert!(
        !tools.iter().any(|tool| tool == "orbit.pipeline.invoke"),
        "the implement disallow list must still exclude orbit.pipeline.invoke: {lane}"
    );
    assert_eq!(
        lane["artifact_attachment"]["owner"],
        "orbit.task.artifact:put"
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
        // Exercise normalization of the new structured findings alongside
        // legacy strings, including the false artifact warning itself.
        "utility_warnings": utility_warnings.iter().enumerate().map(|(index, text)| {
            if index % 2 == 0 {
                json!({"text": text, "holds_promotion": true})
            } else {
                json!(text)
            }
        }).collect::<Vec<_>>(),
        "surface_warnings": [],
        "duplicate_of": null,
        "already_landed": null,
        "user_facing_ui": true,
    });
    let (drain, pilot) = drain_and_pilot(workspace);
    let applied = workspace.action(
        "apply_task_pilot_results",
        json!({
            "run_id": pilot,
            "workspace_path": workspace.repo,
            "prepared": prepared,
            "results": [{"partition_index": 0, "task_ids": [task.id], "tasks": [assessment]}],
            "promotion_authorized": true,
            "drain_promotion": {"run_id": drain},
        }),
    );
    assert_eq!(applied["status"], "succeeded", "{applied}");
    let decision = applied["drain_approval"]
        .as_array()
        .and_then(|decisions| {
            decisions
                .iter()
                .find(|decision| decision["task_id"] == task.id.as_str())
        })
        .unwrap_or_else(|| panic!("no drain approval decision for {}: {applied}", task.id))
        .clone();
    let comments = workspace.runtime.get_task_comments(&task.id).unwrap();
    let audit = super::pilot_audit(&workspace.runtime, &task.id, comments.last().unwrap());
    (audit, decision)
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("expected an array: {value}"))
        .iter()
        .map(|item| {
            item.as_str()
                .or_else(|| item.get("text").and_then(Value::as_str))
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
    let (audit, decision) = apply(&screenshots_only, &screenshots, &[INCIDENT, OPERATOR]);
    assert_eq!(
        strings(&audit["assessment"]["utility_warnings"]),
        vec![OPERATOR.to_string()],
        "a screenshot criterion must drop the false attachment warning and keep an unrelated one: {audit}"
    );
    assert_eq!(decision["decision"], "withhold", "{decision}");
    assert_eq!(decision["classification"], "warnings", "{decision}");
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
    let (audit, decision) = apply(&outside, &task, &[INCIDENT, OPERATOR, UNGRANTED, MIXED]);
    assert_eq!(
        strings(&audit["assessment"]["utility_warnings"]),
        vec![
            OPERATOR.to_string(),
            UNGRANTED.to_string(),
            MIXED.to_string()
        ],
        "the ungranted-tool warning and a mixed warning stay, and the false attachment warning does not: {audit}"
    );
    assert_eq!(decision["decision"], "withhold", "{decision}");
    assert_eq!(decision["classification"], "warnings", "{decision}");
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

/// A capture or rendering gap is a fact about the view, so its warning stays
/// and holds approval. An allowlist-only claim that the lane's handoff
/// contradicts is dropped, and then holds nothing.
fn a_capture_or_render_gap_stays_and_holds_approval_while_an_allowlist_claim_does_not() {
    let gaps = Workspace::new();
    install_real_implement_activity(&gaps);
    let task = ui_task(
        &gaps,
        vec!["Attach screenshots of the changed dashboard view as task artifacts.".into()],
    );
    let (audit, decision) = apply(&gaps, &task, &[INCIDENT, CAPTURE, RENDER]);
    assert_eq!(
        strings(&audit["assessment"]["utility_warnings"]),
        vec![CAPTURE.to_string(), RENDER.to_string()],
        "a capture or rendering gap stays, and the allowlist-only claim is dropped: {audit}"
    );
    assert_eq!(decision["decision"], "withhold", "{decision}");
    assert_eq!(decision["classification"], "warnings", "{decision}");

    let allowlist = Workspace::new();
    install_real_implement_activity(&allowlist);
    let task = ui_task(
        &allowlist,
        vec!["Attach screenshots of the changed dashboard view as task artifacts.".into()],
    );
    let (audit, decision) = apply(&allowlist, &task, &[INCIDENT]);
    assert!(
        strings(&audit["assessment"]["utility_warnings"]).is_empty(),
        "the allowlist-only claim is dropped when the lane can attach: {audit}"
    );
    assert_ne!(decision["classification"], "warnings", "{decision}");
}

/// A criterion that describes the corrected behaviour or documentation of
/// a registered tool — the ORB-23555 and ORB-23556 shapes — names that tool
/// as the change's subject, not as a call the implementer must make. It
/// yields no allowlist finding and holds no promotion.
fn a_tool_described_as_the_change_subject_produces_no_allowlist_finding_or_hold() {
    for criteria in [
        vec![
            "A bound MCP tools/call of orbit.workflow with action ship, at least one task id, and mode local or pr, and no workspace selector, is not refused for a missing workspace selector. It reaches the same next gate as the identical call that omits mode.".to_string(),
            "An orbit.workflow action auto call that carries mode still requires an explicit workspace selector.".to_string(),
        ],
        vec![
            "The canonical shipped skill, its synced plugin copy, and the S-workers sentence in the MCP tool evidence note name mode resize on orbit.workflow action auto, and none of them tell the caller to pass action resize.".to_string(),
        ],
    ] {
        let workspace = Workspace::new();
        install_real_implement_activity(&workspace);
        let task = ui_task(&workspace, criteria);
        let (audit, decision) = apply(&workspace, &task, &[]);
        let validation = strings(&audit["assessment"]["validation_tool_warnings"]);
        assert!(
            validation.is_empty(),
            "a tool described as the change subject must produce no allowlist finding: {validation:?}"
        );
        assert_ne!(
            decision["classification"], "warnings",
            "a subject-only tool mention must hold no promotion: {decision}"
        );
    }
}

/// A passive that asks the implementer to call a backticked tool, such as
/// `` `tool` must be invoked to … `` or `` `tool` is invoked to … ``, is still
/// an invocation request. The `be` between a modal and the participle, and
/// the closing backtick before the verb, must not hide an ungranted tool from
/// the allowlist check.
fn a_passive_invocation_request_still_warns_for_an_ungranted_tool() {
    for criterion in [
        "`orbit.pipeline.invoke` must be invoked to submit the catalog job.",
        "`orbit.pipeline.invoke` shall be invoked to submit the catalog job.",
        "`orbit.pipeline.invoke` should be invoked to submit the catalog job.",
        "`orbit.pipeline.invoke` is invoked to submit the catalog job.",
    ] {
        let workspace = Workspace::new();
        install_real_implement_activity(&workspace);
        let task = ui_task(&workspace, vec![criterion.into()]);
        let (audit, _) = apply(&workspace, &task, &[]);
        let validation = strings(&audit["assessment"]["validation_tool_warnings"]);
        assert!(
            validation.iter().any(|warning| warning.starts_with(
                "acceptance criterion requires `orbit.pipeline.invoke`, which the `agent_implement` allowlist does not grant"
            )),
            "`{criterion}` requires an ungranted tool and must produce the allowlist finding: {validation:?}"
        );
    }
}

crate::scenario_table!(
    screenshot_attachment_is_feasible_and_an_ungranted_tool_still_warns_scenarios,
    screenshot_attachment_is_feasible_and_an_ungranted_tool_still_warns,
    a_capture_or_render_gap_stays_and_holds_approval_while_an_allowlist_claim_does_not,
    a_tool_described_as_the_change_subject_produces_no_allowlist_finding_or_hold,
    a_passive_invocation_request_still_warns_for_an_ungranted_tool,
);
