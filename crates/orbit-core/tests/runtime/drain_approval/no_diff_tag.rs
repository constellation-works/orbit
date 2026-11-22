//! A pilot verdict that a task expects no repository diff tags it
//! `no-diff-expected`, so its empty `context_files` stop blocking admission
//! [ORB-26836]. A verdict that says the work already landed, or a finding task
//! with no selectors, keeps its hold.

use orbit_types::task::{NO_DIFF_EXPECTED_TAG, ReadinessStage, readiness_gaps};

use super::*;

const NO_DIFF_EVIDENCE: &str = "The deliverable is a filed task list and a report artifact.";

fn tags_of(ws: &Workspace, task: &Task) -> Vec<String> {
    ws.runtime.get_task(&task.id).unwrap().tags
}

fn blocking_gaps(ws: &Workspace, task: &Task) -> Vec<String> {
    let task = ws.runtime.get_task(&task.id).unwrap();
    readiness_gaps(
        ReadinessStage::Proposed,
        &task.tags,
        &task.context_files,
        task.complexity,
    )
    .into_iter()
    .filter(|gap| gap.is_blocking())
    .map(|gap| gap.code.as_str().to_string())
    .collect()
}

fn audit_task(ws: &Workspace, title: &str, tags: &[&str]) -> Task {
    ws.task(title, tags, &["file:README.md"], TaskComplexity::Low)
}

fn a_no_diff_verdict_tags_the_task_and_clears_the_context_files_gap() {
    let ws = Workspace::new();
    let task = audit_task(&ws, "audit", &["auto-task:pilot-block-patterns"]);
    let before = ws.runtime.get_task(&task.id).unwrap();
    assert!(!before.tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG));

    apply_verified_outside(&ws, vec![verified_no_diff(&task, NO_DIFF_EVIDENCE)]);

    let after = ws.runtime.get_task(&task.id).unwrap();
    assert_eq!(after.status, TaskStatus::Proposed, "no authority promotes");
    assert!(after.context_files.is_empty(), "{after:?}");
    assert_eq!(
        after
            .tags
            .iter()
            .filter(|tag| *tag == NO_DIFF_EXPECTED_TAG)
            .count(),
        1,
        "{:?}",
        after.tags
    );
    assert!(
        after
            .tags
            .iter()
            .any(|tag| tag == "auto-task:pilot-block-patterns"),
        "the pilot keeps the task's own tags: {:?}",
        after.tags
    );
    assert_eq!(blocking_gaps(&ws, &task), Vec::<String>::new());
}

fn a_drain_approves_the_task_its_pilot_tagged_in_the_same_pass() {
    let ws = Workspace::new();
    let drain = ws.drain();
    let task = audit_task(&ws, "audit", &[]);
    let selection = ws.select(&drain);
    assert_task_ids(&selection, &[&task]);
    let pilot = ws.pilot_child(&drain, &selection["task_ids"]);
    let applied = ws.pilot_ok(
        &drain,
        &pilot,
        vec![verified_no_diff(&task, NO_DIFF_EVIDENCE)],
    );
    assert_classification(&applied, "drain_approval", "verified_ready");
    assert_eq!(applied["drain_approval"][0]["approved"], true, "{applied}");
    assert_status(&ws, &task, TaskStatus::Backlog);
    assert!(tags_of(&ws, &task).contains(&NO_DIFF_EXPECTED_TAG.to_string()));
}

fn a_verdict_that_is_not_a_no_diff_deliverable_leaves_the_task_untagged_and_gapped() {
    let ws = Workspace::new();
    let landed = ws.commit("the fix");
    let cited = audit_task(&ws, "cites a commit", &[]);
    let typed = audit_task(&ws, "typed already landed", &[]);
    let duplicate = audit_task(&ws, "typed duplicate", &[]);
    let release = audit_task(&ws, "release action", &[]);
    let finding = audit_task(&ws, "review finding", &["delivery-code-review"]);
    let unfound = ws.task("no selectors found", &[], &[], TaskComplexity::Low);
    let mut not_found = assessment(&unfound);
    not_found["context_files_after"] = json!([]);
    let mut already_landed = verified_no_diff(&typed, NO_DIFF_EVIDENCE);
    already_landed["already_landed"] = json!({"evidence": "The change is on the base branch."});
    let mut duplicate_of = verified_no_diff(&duplicate, NO_DIFF_EVIDENCE);
    duplicate_of["duplicate_of"] = json!({"task_id": cited.id, "evidence": "Same work."});
    let mut release_action = verified_no_diff(&release, NO_DIFF_EVIDENCE);
    release_action["release_action_required"] =
        json!({"action": "publish the tag", "evidence": "The tag is recorded but unpublished."});
    apply_verified_outside(
        &ws,
        vec![
            verified_no_diff(&cited, &format!("Commit {landed} already carries it.")),
            already_landed,
            duplicate_of,
            release_action,
            verified_no_diff(
                &finding,
                "The failing test does not exist at this revision.",
            ),
        ],
    );
    for task in [&cited, &typed, &duplicate, &release, &finding, &unfound] {
        assert!(
            !tags_of(&ws, task).contains(&NO_DIFF_EXPECTED_TAG.to_string()),
            "{} must stay untagged",
            task.title
        );
        assert_eq!(
            blocking_gaps(&ws, task),
            vec!["missing_context_files".to_string()],
            "{} keeps its gap",
            task.title
        );
    }
}

fn a_task_with_selectors_or_the_tag_is_left_as_it_was() {
    let ws = Workspace::new();
    let scoped = audit_task(&ws, "has context files", &[]);
    let tagged = audit_task(&ws, "already tagged", &[NO_DIFF_EXPECTED_TAG, "chore"]);
    apply_verified_outside(
        &ws,
        vec![
            assessment(&scoped),
            verified_no_diff(&tagged, NO_DIFF_EVIDENCE),
        ],
    );
    let scoped_after = ws.runtime.get_task(&scoped.id).unwrap();
    assert_eq!(scoped_after.context_files, vec!["file:README.md"]);
    assert!(
        !scoped_after
            .tags
            .iter()
            .any(|tag| tag == NO_DIFF_EXPECTED_TAG)
    );
    let mut tags = tags_of(&ws, &tagged);
    tags.sort();
    assert_eq!(
        tags,
        vec!["chore".to_string(), NO_DIFF_EXPECTED_TAG.to_string()]
    );
}

crate::scenario_table!(
    a_no_diff_verdict_tags_the_task_scenarios,
    a_no_diff_verdict_tags_the_task_and_clears_the_context_files_gap,
    a_drain_approves_the_task_its_pilot_tagged_in_the_same_pass,
    a_verdict_that_is_not_a_no_diff_deliverable_leaves_the_task_untagged_and_gapped,
    a_task_with_selectors_or_the_tag_is_left_as_it_was
);
