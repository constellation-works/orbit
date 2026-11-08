//! Public task-pilot boundary for an undecided consequential decision.
//!
//! One fixture carries both sides of the acceptance check: a description
//! that leaves an ownership or policy choice, or an unenumerated surface a
//! stop rule reaches, and a description whose only open choices are ordinary
//! implementation choices. The incidents are an open ownership policy
//! [ORB-21111], a later resolution of that same choice [ORB-23274], and a
//! surface the stop rule reaches that the spec did not enumerate [ORB-21174].

use orbit_core::application::task::TaskAddParams;
use orbit_core::{Task, TaskStatus};
use serde_json::{Value, json};

use super::Workspace;

#[test]
fn undecided_consequential_decision_holds_and_ordinary_choice_does_not() {
    let workspace = Workspace::new();

    let ownership = filed(
        &workspace,
        "Choose whether to preserve the current open-task ownership policy. \
         That ownership policy is undecided.",
    );
    let ownership_audit = assess(&workspace, &ownership, json!([]));
    assert_holds(
        &ownership_audit,
        "the ownership policy",
        "ORB-21111: an open ownership-policy choice must hold promotion",
    );
    assert_public_hold(&workspace, &ownership, "the ownership policy");

    let policy = filed(&workspace, "The retry policy is undecided.");
    let policy_audit = assess(&workspace, &policy, json!([]));
    assert_holds(
        &policy_audit,
        "the policy choice",
        "an open policy choice must hold promotion",
    );
    assert_public_hold(&workspace, &policy, "the policy choice");

    let surface = filed(
        &workspace,
        "Stop on any surface difference the difference table does not enumerate. \
         The difference table omits bounded description text.",
    );
    let surface_audit = assess(&workspace, &surface, json!([]));
    assert_holds(
        &surface_audit,
        "bounded description text",
        "ORB-21174: an unenumerated surface the stop rule reaches must hold promotion",
    );
    assert_public_hold(&workspace, &surface, "bounded description text");

    let ordinary = filed(
        &workspace,
        "The implementer chooses the file split, the helper's name and the test placement. \
         The naming policy for tests is an ordinary implementation choice the implementer decides.",
    );
    let ordinary_audit = assess(
        &workspace,
        &ordinary,
        json!([{
            "text": "The file split is an implementer choice.",
            "holds_promotion": false
        }]),
    );
    assert_no_consequential_hold(
        &ordinary_audit,
        "an ordinary implementation choice must not hold promotion",
    );
    let advisory = &ordinary_audit["assessment"]["utility_warnings"][0];
    assert_eq!(advisory["holds_promotion"], false, "{ordinary_audit}");
    assert!(advisory.get("hold").is_none(), "{ordinary_audit}");
    assert_public_clear(&workspace, &ordinary);

    let code_health = filed(
        &workspace,
        "A behavioural difference between copies that this task does not list is a \
         stop-and-report, not a judgement call.",
    );
    assert_no_consequential_hold(
        &assess(&workspace, &code_health, json!([])),
        "a stop-and-report that is the specified handling must not hold",
    );
    assert_public_clear(&workspace, &code_health);

    let settled_surface = filed(
        &workspace,
        "Stop on any surface difference the difference table does not enumerate. \
         The difference table omits bounded description text. \
         Any field the table does not name keeps its current behavior and the implementer \
         does not stop for it.",
    );
    assert_no_consequential_hold(
        &assess(&workspace, &settled_surface, json!([])),
        "ORB-21174: a surface the spec resolves must not hold",
    );
    assert_public_clear(&workspace, &settled_surface);

    let settled_ownership = filed(
        &workspace,
        "Preserve the current open-task ownership policy.",
    );
    assert_no_consequential_hold(
        &assess(&workspace, &settled_ownership, json!([])),
        "ORB-21111: a stated ownership policy must not hold",
    );
    assert_public_clear(&workspace, &settled_ownership);

    let resolved_after_open = filed(
        &workspace,
        "The ownership policy was undecided. Preserve the current ownership policy.",
    );
    assert_no_consequential_hold(
        &assess(&workspace, &resolved_after_open, json!([])),
        "ORB-23274: a later preserve of the same ownership policy must not hold",
    );
    assert_public_clear(&workspace, &resolved_after_open);

    let resolved_before_open = filed(
        &workspace,
        "Preserve the current ownership policy. The ownership policy was undecided.",
    );
    assert_holds(
        &assess(&workspace, &resolved_before_open, json!([])),
        "the ownership policy",
        "a resolution before the open marker must not clear the hold",
    );
    assert_public_hold(&workspace, &resolved_before_open, "the ownership policy");

    let negated_resolution = filed(
        &workspace,
        "The ownership policy was undecided. Do not preserve the current ownership policy.",
    );
    assert_holds(
        &assess(&workspace, &negated_resolution, json!([])),
        "the ownership policy",
        "do not preserve must not count as a resolution",
    );
    assert_public_hold(&workspace, &negated_resolution, "the ownership policy");

    let never_resolution = filed(
        &workspace,
        "The ownership policy was undecided. Never preserve the current ownership policy.",
    );
    assert_holds(
        &assess(&workspace, &never_resolution, json!([])),
        "the ownership policy",
        "never preserve must not count as a resolution",
    );
    assert_public_hold(&workspace, &never_resolution, "the ownership policy");

    let adverb_negated_resolution = filed(
        &workspace,
        "The ownership policy was undecided. Do not automatically preserve the current ownership policy.",
    );
    assert_holds(
        &assess(&workspace, &adverb_negated_resolution, json!([])),
        "the ownership policy",
        "do not automatically preserve must not count as a resolution",
    );
    assert_public_hold(
        &workspace,
        &adverb_negated_resolution,
        "the ownership policy",
    );

    let never_keep = filed(
        &workspace,
        "The ownership policy was undecided. Never keep the current ownership policy.",
    );
    assert_holds(
        &assess(&workspace, &never_keep, json!([])),
        "the ownership policy",
        "never keep must not count as a resolution",
    );
    assert_public_hold(&workspace, &never_keep, "the ownership policy");

    let adverb_negated_retain = filed(
        &workspace,
        "The ownership policy was undecided. Do not automatically retain the current ownership policy.",
    );
    assert_holds(
        &assess(&workspace, &adverb_negated_retain, json!([])),
        "the ownership policy",
        "do not automatically retain must not count as a resolution",
    );
    assert_public_hold(&workspace, &adverb_negated_retain, "the ownership policy");

    let affirmative_adverb_resolution = filed(
        &workspace,
        "The ownership policy was undecided. Automatically preserve the current ownership policy.",
    );
    assert_no_consequential_hold(
        &assess(&workspace, &affirmative_adverb_resolution, json!([])),
        "an affirmative adverbial preserve must still clear the hold",
    );
    assert_public_clear(&workspace, &affirmative_adverb_resolution);

    let different_choice = filed(
        &workspace,
        "The retry policy is undecided. Preserve the current ownership policy.",
    );
    assert_holds(
        &assess(&workspace, &different_choice, json!([])),
        "the policy choice",
        "preserving a different choice must not clear an open policy",
    );
    assert_public_hold(&workspace, &different_choice, "the policy choice");

    let unrelated_policy = filed(
        &workspace,
        "The retry policy is undecided. Preserve the current security policy.",
    );
    assert_holds(
        &assess(&workspace, &unrelated_policy, json!([])),
        "the policy choice",
        "ORB-23288: a different policy must not clear the open retry policy",
    );
    assert_public_hold(&workspace, &unrelated_policy, "the policy choice");

    let resolved_policy = filed(
        &workspace,
        "The retry policy is undecided. Preserve the current retry policy.",
    );
    assert_no_consequential_hold(
        &assess(&workspace, &resolved_policy, json!([])),
        "ORB-23288: a later resolution of the same policy must clear its hold",
    );
    assert_public_clear(&workspace, &resolved_policy);

    let split_sentence = filed(
        &workspace,
        "Document the ownership policy. The format is undecided.",
    );
    assert_no_consequential_hold(
        &assess(&workspace, &split_sentence, json!([])),
        "an open word in a different sentence from the choice must not hold",
    );
    assert_public_clear(&workspace, &split_sentence);

    let upgraded = filed(
        &workspace,
        "Choose whether to preserve the current open-task ownership policy.",
    );
    let upgraded_audit = assess(
        &workspace,
        &upgraded,
        json!([{
            "text": "The ownership policy can be chosen during implementation.",
            "holds_promotion": false
        }]),
    );
    assert_eq!(
        upgraded_audit["assessment"]["utility_warnings"]
            .as_array()
            .map(Vec::len),
        Some(1),
        "a model advisory that names the decision is stamped, not duplicated: {upgraded_audit}"
    );
    assert_holds(
        &upgraded_audit,
        "the ownership policy",
        "a model advisory on the same open ownership policy must be upgraded to a hold",
    );
    assert_public_hold(&workspace, &upgraded, "the ownership policy");
}

#[test]
fn unrelated_resolution_cue_does_not_clear_surface_hold() {
    let workspace = Workspace::new();

    // ORB-23275: "retains current" in a sentence that does not mention the
    // unlisted surface must not suppress the hold on the omitted surface.
    let unrelated = filed(
        &workspace,
        "Stop on any surface difference the difference table does not enumerate. \
         The difference table omits bounded description text. \
         The retry timeout retains current behavior.",
    );
    assert_holds(
        &assess(&workspace, &unrelated, json!([])),
        "bounded description text",
        "ORB-23275: an unrelated resolution cue must not clear an unenumerated surface hold",
    );
    assert_public_hold(&workspace, &unrelated, "bounded description text");
}

fn filed(workspace: &Workspace, description: &str) -> Task {
    workspace
        .runtime
        .add_task(TaskAddParams {
            title: "Consequential decision fixture".into(),
            description: description.into(),
            acceptance_criteria: vec!["The fixture repair is observable.".into()],
            plan: "Inspect README.md.".into(),
            status: Some(TaskStatus::Proposed),
            context_files: vec!["file:README.md".into()],
            ..Default::default()
        })
        .unwrap()
}

fn assess(workspace: &Workspace, task: &Task, utility_warnings: Value) -> Value {
    let prepared = workspace.prepare(&[&task.id]);
    let current = workspace.runtime.get_task(&task.id).unwrap();
    assert_eq!(current.status, TaskStatus::Proposed);
    let assessment = json!({
        "task_id": task.id,
        "context_files_before": current.context_files,
        "context_files_after": ["file:README.md"],
        "disposition": "selectors",
        "recommended_crew": "fixture",
        "recommended_complexity": "low",
        "confidence": "high",
        "assessment_rationale": "README.md contains the affected material.",
        "validation_approach": "Exercise the task-pilot hold boundary.",
        "evidence_gaps": [],
        "reassessment_triggers": [],
        "blocked_by": [],
        "adr_conflicts": [],
        "utility_warnings": utility_warnings,
        "surface_warnings": [],
        "duplicate_of": null,
        "already_landed": null,
    });
    let applied = workspace.action(
        "apply_task_pilot_results",
        json!({
            "workspace_path": workspace.repo,
            "prepared": prepared,
            "results": [{
                "partition_index": 0,
                "task_ids": [task.id],
                "tasks": [assessment],
            }],
        }),
    );
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(applied["applied_count"], 1, "{applied}");
    assert_eq!(
        workspace.runtime.get_task(&task.id).unwrap().status,
        TaskStatus::Proposed,
        "apply records the hold and leaves the task proposed"
    );
    let comments = workspace.runtime.get_task_comments(&task.id).unwrap();
    serde_json::from_str(comments.last().unwrap().receipt().unwrap().body).unwrap()
}

fn assert_holds(audit: &Value, decision: &str, message: &str) {
    let warnings = audit["assessment"]["utility_warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("{message}: {audit}"));
    let finding = warnings
        .iter()
        .find(|finding| {
            finding["text"]
                .as_str()
                .is_some_and(|text| text.to_ascii_lowercase().contains(decision))
        })
        .unwrap_or_else(|| panic!("{message}: no finding names {decision}: {audit}"));
    assert_eq!(finding["holds_promotion"], true, "{message}: {finding}");
    assert_eq!(
        finding["hold"], "undecided_consequential_decision",
        "{message}: {finding}"
    );
}

fn assert_no_consequential_hold(audit: &Value, message: &str) {
    let warnings = audit["assessment"]["utility_warnings"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        warnings.iter().all(|finding| {
            finding.get("hold").and_then(Value::as_str) != Some("undecided_consequential_decision")
                && finding.get("holds_promotion").and_then(Value::as_bool) != Some(true)
        }),
        "{message}: {warnings:?}"
    );
}

fn assert_public_hold(workspace: &Workspace, task: &Task, decision: &str) {
    let findings = public_warnings(workspace, task);
    assert!(
        findings.iter().any(|finding| {
            finding.kind == "utility_warnings"
                && finding.text.to_ascii_lowercase().contains(decision)
        }),
        "task_pilot_warnings must name {decision} for a proposed task: {findings:?}"
    );
}

fn assert_public_clear(workspace: &Workspace, task: &Task) {
    let findings = public_warnings(workspace, task);
    assert!(
        findings.is_empty(),
        "task_pilot_warnings must be empty when no consequential decision is open: {findings:?}"
    );
}

fn public_warnings(
    workspace: &Workspace,
    task: &Task,
) -> Vec<orbit_types::workflow::DrainHoldFinding> {
    let comments = workspace.runtime.get_task_comments(&task.id).unwrap();
    orbit_core::application::task::task_pilot_warnings(
        TaskStatus::Proposed,
        &comments,
        &[],
        &|_| false,
    )
}
