//! Owner audit/cursor requirements are machine findings, not tool-name grants.

use super::*;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_types::task::TaskStatus;

const CRITERION: &str = "Scan the authoritative workspace tool audit strictly after the completed predecessor's cursor; preserve that cursor if its evidence cannot be read.";
const CURSOR_PATH: &str = "tool-failure-cursor.json";
const CURSOR: &[u8] = br#"{"schema_version":1,"cursor":"2026-10-20T07:40:00Z","events_scanned":7}"#;

fn finding() -> Value {
    json!({"required_machine": [{
        "criterion": 1, "machine": OWNER,
        "evidence": "The authoritative owner audit and completed predecessor cursor cannot be substituted with the follower audit or read through the task-scoped bridge.",
    }]})
}

fn scan() -> (Pair, String) {
    let mut pair = Pair::new(0);
    let predecessor = pair
        .wire
        .owner
        .add_task(TaskAddParams {
            title: "Completed audit scan".into(),
            description: "Historical cursor fixture.".into(),
            status: Some(TaskStatus::Review),
            ..Default::default()
        })
        .unwrap();
    pair.wire
        .owner
        .update_task_as_human(
            &predecessor.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                execution_summary: Some("Cursor: 2026-10-20T07:40:00Z".into()),
                upsert_artifacts: vec![TaskArtifact {
                    path: CURSOR_PATH.into(),
                    content: CURSOR.to_vec(),
                    media_type: "application/json".into(),
                    created_by: None,
                }],
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    let id = backlog_task_with_input(
        &pair.wire.owner,
        &pair.owner_repo,
        "src/f0.rs",
        None,
        json!({
            "title": "Scan workspace failures",
            "description": format!("Read the authoritative workspace audit and completed predecessor {}'s cursor artifact, falling back to its summary only. Never substitute another audit or guess the window.", predecessor.id),
            "acceptance_criteria": [CRITERION],
            "required_tools": ["orbit.task.artifact.get", "orbit.task.list", "orbit.task.show"],
        }),
    );
    pair.tasks.push(id);
    (pair, predecessor.id)
}

fn assert_cursor(pair: &Pair, predecessor: &str) {
    let artifact = pair
        .wire
        .owner
        .get_task_artifact(predecessor, CURSOR_PATH)
        .unwrap()
        .unwrap();
    assert_eq!(
        artifact.content, CURSOR,
        "admission never advances the predecessor cursor"
    );
    assert_eq!(pair.owner_status(predecessor), "done");
}

fn assert_owner_eligible(pair: &Pair, predecessor: &str) {
    let selected = action(pair, "list_backlog_tasks", json!({}));
    assert!(
        selected["task_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(pair.tasks[0])),
        "{selected}"
    );
    let summary = pair
        .wire
        .owner
        .run_tool(
            "orbit.task.show",
            json!({"id": predecessor, "field": "execution_summary"}),
        )
        .unwrap();
    assert_eq!(
        summary, "Cursor: 2026-10-20T07:40:00Z",
        "the owner retains authorized predecessor reads"
    );
    assert_cursor(pair, predecessor);
}

fn assert_follower_held(pair: &Pair, drain: &str, pass: &Value) {
    assert!(error_of(pass).is_empty(), "{pass}");
    assert_eq!(pass["admitted"], 0, "{pass}");
    assert!(pair.owner_claims().is_empty(), "no follower claim");
    assert!(pair.leaf_runs().is_empty(), "no leaf can set up a worktree");
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
    let (reason, detail) = recorded_wait(pair, drain, &pair.tasks[0]);
    assert_eq!(reason, "owner_hold", "{detail}");
    assert!(
        detail.starts_with("Machine requirement: criterion 1"),
        "{detail}"
    );
    assert_eq!(
        pair.owner_task(&pair.tasks[0])["required_tools"],
        json!([
            "orbit.task.artifact.get",
            "orbit.task.list",
            "orbit.task.show"
        ])
    );
}

fn owner_evidence_requires_a_typed_finding_despite_supported_tool_names() {
    // Reproduce the missing finding: tool names alone admit this contract
    // (F2026-10-531). The broker boundary suite separately proves that its
    // scoped reads refuse a predecessor; an in-process owner route is broader.
    let (gap, predecessor) = scan();
    apply_assessment(&gap, CRITERION, json!({}));
    let drain = gap.start_drain();
    let leaf = gap.queued_leaf(&drain, 1);
    assert_eq!(gap.claimed_task(&leaf), gap.tasks[0]);
    assert_cursor(&gap, &predecessor);

    let (pair, predecessor) = scan();
    let prepared = prepare(&pair, &pair.tasks[0]);
    assert_eq!(prepared["owner_machine"]["machine_id"], OWNER);
    apply_assessment(&pair, CRITERION, finding());
    assert_owner_eligible(&pair, &predecessor);
    let drain = pair.start_drain();
    let pass = pair.pass(&drain);
    assert_follower_held(&pair, &drain, &pass);
    assert_cursor(&pair, &predecessor);

    // The same bridged names remain portable when all required reads fit
    // their scopes. No scan-title special case or tool mutation is involved.
    let portable = backlog_task_with_input(
        &pair.wire.owner,
        &pair.owner_repo,
        "src/portable.rs",
        None,
        json!({
            "title": "Scan workspace failures",
            "acceptance_criteria": ["List owner tasks and read only this task's artifacts."],
            "required_tools": ["orbit.task.artifact.get", "orbit.task.list", "orbit.task.show"],
        }),
    );
    let leaf = pair.queued_leaf(&drain, 1);
    assert_eq!(pair.claimed_task(&leaf), portable);
    assert!(!pair.owner_has_claim_for_task(&pair.tasks[0]));
    assert_cursor(&pair, &predecessor);
}

fn owner_evidence_applied_after_selection_is_rechecked_before_claim() {
    let (pair, predecessor) = scan();
    let input = assessment_input(&pair, CRITERION, finding());
    let owner = pair.wire.owner.clone();
    let drain = pair.start_drain();
    let pass = orbit_core::test_support::with_pull_admission_checkpoint(
        move || {
            let output = owner
                .run_deterministic(
                    "apply_task_pilot_results",
                    &json!({}),
                    &input,
                    ToolContext::default(),
                )
                .unwrap();
            assert_eq!(output["applied_count"], 1, "{output}");
        },
        || pair.pass(&drain),
    );
    assert_follower_held(&pair, &drain, &pass);
    assert_owner_eligible(&pair, &predecessor);
}

fn an_owner_scan_still_requires_an_independent_operator_handoff() {
    let (pair, predecessor) = scan();
    let mut findings = finding();
    findings["disposition"] = json!("host_operational");
    findings["context_files_after"] = json!([]);
    findings["evidence"] = json!(
        "The operator must re-home and re-author this report's evidence contract before execution."
    );
    apply_assessment(&pair, CRITERION, findings);
    let selected = action(&pair, "list_backlog_tasks", json!({}));
    assert!(
        !selected["task_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(pair.tasks[0])),
        "owner routing does not release the handoff: {selected}"
    );
    let drain = pair.start_drain();
    let pass = pair.pass(&drain);
    assert!(error_of(&pass).is_empty(), "{pass}");
    assert!(pair.owner_claims().is_empty());
    assert!(pair.leaf_runs().is_empty());
    let (_, detail) = recorded_wait(&pair, &drain, &pair.tasks[0]);
    assert!(detail.contains("host_operational"), "{detail}");
    assert_cursor(&pair, &predecessor);
}

crate::scenario_table!(
    owner_evidence_admission_scenarios,
    owner_evidence_requires_a_typed_finding_despite_supported_tool_names,
    owner_evidence_applied_after_selection_is_rechecked_before_claim,
    an_owner_scan_still_requires_an_independent_operator_handoff
);
