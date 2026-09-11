//! Preparation and admission coverage for the validation-tool feasibility
//! check [ORB-11980], driven by the two frictions that motivated it:
//! F2026-09-065 (a criterion requiring `proc.spawn` over MCP, which is
//! CLI-only) and F2026-09-066 (a criterion requiring live workflow observation
//! the implementation lane neither allowlists nor has the capability for).

use std::collections::BTreeSet;

use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};
use serde_json::{Value, json};

use super::super::task_pilot::{apply, member_ready, prepare};
use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_layout, write_workspace_file,
};
use crate::application::task::TaskAddParams;

/// The lane a validation criterion has to be feasible in is the shipped
/// implementation activity, so fixtures seed the real asset rather than a
/// hand-written allowlist that could drift from it.
fn seed_implementation_activity(root: &tempfile::TempDir) {
    let activities = root.path().join("home/.orbit/resources/activities");
    std::fs::create_dir_all(&activities).expect("global activities dir");
    let &(_, yaml) = crate::runtime::assets::DEFAULT_ACTIVITY_FILES
        .iter()
        .find(|(name, _)| *name == "agent_implement")
        .expect("shipped agent_implement asset");
    std::fs::write(activities.join("agent_implement.yaml"), yaml)
        .expect("seed the implementation activity");
}

fn seed_task(
    runtime: &OrbitRuntime,
    title: &str,
    acceptance_criteria: &[&str],
    required_tools: &[&str],
) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: acceptance_criteria
                .iter()
                .map(|criterion| (*criterion).to_string())
                .collect(),
            required_tools: required_tools
                .iter()
                .map(|tool| (*tool).to_string())
                .collect(),
            plan: "Inspect and update the fixture.".to_string(),
            priority: TaskPriority::Medium,
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::Backlog),
            ..TaskAddParams::default()
        })
        .expect("seed task")
}

fn prepared(runtime: &OrbitRuntime, repo_root: &std::path::Path, task_ids: &[String]) -> Value {
    prepare(
        runtime,
        "prepare_task_pilot",
        &json!({
            "task_ids": task_ids,
            "workspace_path": repo_root,
        }),
    )
    .expect("prepare explicit task-pilot selection")
}

/// Findings prepared for `task_id`, as plain strings.
fn findings(prepared: &Value, task_id: &str) -> Vec<String> {
    prepared["tasks"]
        .as_array()
        .expect("prepared tasks")
        .iter()
        .find(|snapshot| snapshot["task_id"] == json!(task_id))
        .expect("prepared snapshot for task")["validation_tool_warnings"]
        .as_array()
        .expect("validation_tool_warnings array")
        .iter()
        .map(|finding| finding.as_str().expect("finding is a string").to_string())
        .collect()
}

fn canonical_mcp_tool_names() -> BTreeSet<String> {
    orbit_tools::canonical_builtin_mcp_tool_definitions()
        .expect("canonical MCP tool definitions")
        .into_iter()
        .map(|definition| definition.schema.name)
        .collect()
}

/// F2026-09-065 and F2026-09-066, side by side: both criteria are infeasible
/// in the implementation lane, and preparation — which runs before any pilot
/// worker launches — has to say so for distinct, actionable reasons.
#[test]
fn preparation_reports_distinct_reasons_for_mcp_transport_and_ungranted_observation() {
    let (root, runtime, repo_root) = runtime_with_workspace_layout();
    seed_implementation_activity(&root);

    let mcp_transport = seed_task(
        &runtime,
        "signal handling",
        &[
            "Start `orbit mcp listen`, call `proc.spawn` over that MCP session with a 30-second sleep, SIGTERM the server, and confirm it exits inside the grace period.",
        ],
        &[],
    );
    let live_observation = seed_task(
        &runtime,
        "run observability",
        &["Verify the installed run through `orbit.workflow.run.show` against the live workspace."],
        &[],
    );

    let prepared = prepared(
        &runtime,
        &repo_root,
        &[mcp_transport.id.clone(), live_observation.id.clone()],
    );

    let transport = findings(&prepared, &mcp_transport.id);
    assert_eq!(transport.len(), 1, "unexpected findings: {transport:?}");
    assert!(
        transport[0].contains("`proc.spawn`")
            && transport[0].contains("over MCP")
            && transport[0].contains("orbit tool run proc.spawn"),
        "transport finding must name the tool and the lane that can run it: {transport:?}"
    );

    let observation = findings(&prepared, &live_observation.id);
    assert_eq!(observation.len(), 2, "unexpected findings: {observation:?}");
    assert!(
        observation.iter().any(
            |finding| finding.contains("`required_tools` does not declare")
                && finding.contains("immutable")
        ),
        "one finding must name the missing declaration: {observation:?}"
    );
    assert!(
        observation
            .iter()
            .any(|finding| finding.contains("operator") && finding.contains("operator handoff")),
        "one finding must route operator-reserved validation to a handoff: {observation:?}"
    );

    // The two tasks fail for different reasons, so neither finding may be a
    // generic "this tool is a problem" message reused for both.
    assert!(
        observation
            .iter()
            .all(|finding| !transport.contains(finding)),
        "findings must be distinct: {transport:?} vs {observation:?}"
    );
}

/// A tool the lane really supports — whether from the activity baseline or
/// from an explicit `required_tools` declaration — has to pass cleanly, or the
/// check is noise. Declaration is still only allowlist membership: it supplies
/// neither the operator capability nor an external credential.
#[test]
fn declaration_satisfies_the_allowlist_but_not_capability_or_credentials() {
    let (root, runtime, repo_root) = runtime_with_workspace_layout();
    seed_implementation_activity(&root);

    // The same criterion twice, declared and undeclared, so a clean result
    // proves the declaration carried it rather than the check overlooking the
    // tool.
    const AUTO_TASK_LIST: &str = "Enumerate the workspace's auto-task definitions with `orbit.auto_task.list` and confirm the seeded definition appears.";

    let supported = seed_task(
        &runtime,
        "supported tools",
        &[
            "Read the task with `orbit.task.show` and confirm the plan field is populated.",
            AUTO_TASK_LIST,
        ],
        &["orbit.auto_task.list"],
    );
    let undeclared = seed_task(&runtime, "undeclared tool", &[AUTO_TASK_LIST], &[]);
    let operator_reserved = seed_task(
        &runtime,
        "declared operator tool",
        &[
            "List the workspace's runs with `orbit.workflow.run.list` and compare the newest run id.",
        ],
        &["orbit.workflow.run.list"],
    );
    let external_credential = seed_task(
        &runtime,
        "declared GitHub read",
        &["Confirm the failing job appears in `github.run.list` output for the tested commit."],
        &["github.run.list"],
    );

    let prepared = prepared(
        &runtime,
        &repo_root,
        &[
            supported.id.clone(),
            undeclared.id.clone(),
            operator_reserved.id.clone(),
            external_credential.id.clone(),
        ],
    );

    assert_eq!(
        findings(&prepared, &supported.id),
        Vec::<String>::new(),
        "a baseline tool and a declared tool must both pass"
    );

    let missing_declaration = findings(&prepared, &undeclared.id);
    assert_eq!(
        missing_declaration.len(),
        1,
        "unexpected findings: {missing_declaration:?}"
    );
    assert!(
        missing_declaration[0].contains("`orbit.auto_task.list`")
            && missing_declaration[0].contains("`required_tools` does not declare"),
        "the same criterion without the declaration must report it: {missing_declaration:?}"
    );

    let capability = findings(&prepared, &operator_reserved.id);
    assert_eq!(capability.len(), 1, "unexpected findings: {capability:?}");
    assert!(
        capability[0].contains("operator") && capability[0].contains("allowlist membership only"),
        "declaring an operator-reserved tool must not read as satisfied: {capability:?}"
    );

    let credential = findings(&prepared, &external_credential.id);
    assert_eq!(credential.len(), 1, "unexpected findings: {credential:?}");
    assert!(
        credential[0].contains("GitHub authentication")
            && credential[0].contains("cannot supply credentials"),
        "a declared GitHub read must still name its credential precondition: {credential:?}"
    );
}

/// Prose is evidence, not authority. A tool named inside a quoted example is
/// not a requirement, a criterion that expects a refusal is a correct negative
/// test, and neither may grant a tool or fail preparation.
#[test]
fn quoted_examples_and_negative_tests_neither_grant_nor_reject() {
    let (root, runtime, repo_root) = runtime_with_workspace_layout();
    seed_implementation_activity(&root);

    let quoted_example = seed_task(
        &runtime,
        "quoted example",
        &[
            r#"The captured artifact records the string "orbit.workflow.run.show was skipped for this lane" verbatim."#,
        ],
        &[],
    );
    let negative_test = seed_task(
        &runtime,
        "negative test",
        &["The lane refuses `orbit.workflow.run.show`, and the fixture asserts that refusal."],
        &[],
    );

    let prepared = prepared(
        &runtime,
        &repo_root,
        &[quoted_example.id.clone(), negative_test.id.clone()],
    );

    assert_eq!(
        findings(&prepared, &quoted_example.id),
        Vec::<String>::new(),
        "a name inside a quoted example is not a validation requirement"
    );
    assert_eq!(
        findings(&prepared, &negative_test.id),
        Vec::<String>::new(),
        "a criterion that expects a refusal is already correct"
    );

    // Neither task's `required_tools` grew, and preparation still selected
    // both: prose can warn, but it can neither grant nor reject.
    for task_id in [&quoted_example.id, &negative_test.id] {
        assert_eq!(
            runtime
                .get_task(task_id)
                .expect("reload fixture task")
                .required_tools,
            Vec::<String>::new()
        );
    }
    assert_eq!(prepared["task_count"], 2);
}

/// The MCP dimension must answer from the same definition set MCP advertises
/// from, so a tool that gains or loses exposure changes this check with it.
#[test]
fn mcp_findings_read_the_canonical_exposure_data() {
    let (root, runtime, repo_root) = runtime_with_workspace_layout();
    seed_implementation_activity(&root);

    let exposed = canonical_mcp_tool_names();
    assert!(
        !exposed.contains("proc.spawn"),
        "fixture assumes `proc.spawn` stays off the MCP surface"
    );
    assert!(
        exposed.contains("orbit.task.show"),
        "fixture assumes `orbit.task.show` stays on the MCP surface"
    );

    let advertised = seed_task(
        &runtime,
        "advertised over MCP",
        &["Call `orbit.task.show` over the MCP session and confirm the envelope shape."],
        &[],
    );
    let cli_only = seed_task(
        &runtime,
        "CLI-only",
        &["Call `proc.spawn` over the MCP session and confirm the child is supervised."],
        &[],
    );

    let prepared = prepared(
        &runtime,
        &repo_root,
        &[advertised.id.clone(), cli_only.id.clone()],
    );

    assert_eq!(
        findings(&prepared, &advertised.id),
        Vec::<String>::new(),
        "an MCP-advertised tool named over MCP is feasible"
    );
    assert_eq!(
        findings(&prepared, &cli_only.id).len(),
        1,
        "a tool absent from the canonical definitions is not reachable over MCP"
    );
}

/// Admission reads the finding, not just preparation's output: the pilot never
/// sees it, so apply attaches it to the assessment it reports and readiness
/// withholds the task.
#[test]
fn admission_withholds_a_task_whose_validation_criteria_are_infeasible() {
    let (root, runtime, repo_root) = runtime_with_workspace_layout();
    seed_implementation_activity(&root);
    write_workspace_file(&repo_root, "src/listen.rs");

    let task = seed_task(
        &runtime,
        "signal handling",
        &["Verify the installed run through `orbit.workflow.run.show` against the live workspace."],
        &[],
    );
    let task_ids = vec![task.id.clone()];
    let prepared_snapshot = prepared(&runtime, &repo_root, &task_ids);

    let output = apply(
        &runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": prepared_snapshot,
            "results": [json!({
                "partition_index": 0,
                "task_ids": task_ids,
                "tasks": [json!({
                    "task_id": task.id,
                    "context_files_before": [],
                    "context_files_after": ["file:src/listen.rs"],
                    "disposition": "selectors",
                    "recommended_crew": "luna",
                    "recommended_complexity": "medium",
                    "assessment_rationale": "The repair is bounded to the named validation surface.",
                    "confidence": "high",
                    "evidence_gaps": [],
                    "validation_approach": "Run the declared validation command.",
                    "reassessment_triggers": ["the acceptance criteria change"],
                    "blocked_by": [],
                    "duplicate_of": null,
                    "already_landed": null,
                    "adr_conflicts": [],
                    "utility_warnings": [],
                    "surface_warnings": [],
                })],
                "summary": "fixture partition",
            })],
            "workspace_path": repo_root,
        }),
    )
    .expect("apply validated pilot results");

    assert_eq!(output["status"], "succeeded");
    let assessment = &output["tasks"][0];
    assert_eq!(
        assessment["validation_tool_warnings"]
            .as_array()
            .expect("reported findings")
            .len(),
        2,
        "apply must report the findings the pilot could not see: {assessment:?}"
    );
    assert!(
        !member_ready(assessment),
        "a task whose acceptance check the lane cannot run is not ready for automatic promotion"
    );

    // Selectors still apply: the finding withholds promotion, it does not
    // discard the pilot's validated work.
    assert_eq!(
        runtime
            .get_task(&task.id)
            .expect("reload task")
            .context_files,
        vec!["file:src/listen.rs"]
    );
}

/// An assessment that predates the injected field reads as "no finding"
/// rather than as "not ready", so readiness stays governed by the pilot's own
/// findings when the deterministic one is absent.
#[test]
fn readiness_treats_an_absent_finding_as_feasible() {
    let mut assessment = json!({
        "task_id": "ORB-FIXTURE",
        "disposition": "selectors",
        "context_files_after": ["file:src/existing.rs"],
        "recommended_complexity": "medium",
        "blocked_by": [],
        "duplicate_of": null,
        "already_landed": null,
        "release_action_required": null,
        "adr_conflicts": [],
        "utility_warnings": [],
        "surface_warnings": [],
    });
    assert!(member_ready(&assessment));

    assessment["validation_tool_warnings"] = json!([]);
    assert!(member_ready(&assessment));

    assessment["validation_tool_warnings"] =
        json!(["acceptance criterion requires `orbit.workflow.run.show`"]);
    assert!(!member_ready(&assessment));
}
