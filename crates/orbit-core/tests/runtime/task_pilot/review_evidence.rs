//! A review-filed task's selectors are the review's evidence: a pilot that
//! proposes only its modification targets cannot erase them, re-assessment
//! does not rewrite the scope, and the over-attachment finding still fires
//! for a swept proposal. Fixtures drive the real prepare and apply actions
//! against a real store and a real repository, in isolated children.

use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{Task, TaskStatus};
use serde_json::{Value, json};

use super::Workspace;

/// The shape of a filed finding: the source paths its evidence cites, in
/// the order the review filed them, plus the regression-test location.
const DUPLICATES: &str = "file:src/dependabot/duplicates.rs";
const FILING: &str = "file:src/dependabot/filing.rs";
const ADMISSION: &str = "file:src/admission/duplicate_tasks.rs";
const CODE_GROUPS: &str = "file:src/dependabot/code_groups.rs";
const SWEEP_TEST: &str = "file:tests/runtime/security_alert_sweep.rs";
const FILED: [&str; 5] = [DUPLICATES, FILING, ADMISSION, CODE_GROUPS, SWEEP_TEST];
/// What a pilot proposing only the edit targets keeps.
const NARROWED: [&str; 2] = [ADMISSION, SWEEP_TEST];
const EXTRA: &str = "file:src/dependabot/mod.rs";

impl Workspace {
    fn commit_files(&self, selectors: &[&str]) {
        for selector in selectors {
            let path = self.repo.join(selector.trim_start_matches("file:"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "fn fixture() {}\n").unwrap();
        }
        self.git(&["add", "."]);
        self.git(&["commit", "-m", "fixture sources"]);
    }

    fn filed_task(&self, title: &str, tags: &[&str], context_files: &[&str]) -> Task {
        self.runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: format!("Repair {title}."),
                acceptance_criteria: vec!["The duplicate is suppressed once.".into()],
                plan: "Inspect the cited sources.".into(),
                status: Some(TaskStatus::Proposed),
                tags: owned(tags),
                context_files: owned(context_files),
                ..Default::default()
            })
            .unwrap()
    }

    fn assess(&self, prepared: &Value, after: &[&str], complexity: &str) -> Value {
        let task_id = prepared["task_ids"][0].clone();
        self.action(
            "apply_task_pilot_results",
            json!({
                "workspace_path": self.repo,
                "prepared": prepared,
                "results": [{
                    "partition_index": 0, "task_ids": [task_id],
                    "tasks": [{
                        "task_id": task_id, "context_files_after": after,
                        "disposition": "selectors", "recommended_crew": "fixture",
                        "recommended_complexity": complexity, "confidence": "high",
                        "assessment_rationale": "The cited sources contain the defect.",
                        "validation_approach": "Inspect the persisted scope.",
                        "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
                        "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
                        "duplicate_of": null, "already_landed": null,
                    }],
                }],
            }),
        )
    }

    fn scope(&self, task_id: &str) -> Vec<String> {
        self.runtime.get_task(task_id).unwrap().context_files
    }
}

fn owned(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn outcome(applied: &Value) -> &Value {
    &applied["task_outcomes"][0]
}

#[test]
fn review_evidence_survives_narrowing_reassessment_and_replay() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::review_evidence::review_evidence_survives_narrowing_reassessment_and_replay",
    ) {
        return;
    }
    let workspace = Workspace::new();
    workspace.commit_files(&[&FILED[..], &[EXTRA]].concat());
    let task = workspace.filed_task("duplicate alert", &["delivery-code-review"], &FILED);

    // The pilot proposes only what it would edit; apply keeps the evidence
    // it omitted, in the order the review filed it.
    let prepared = workspace.prepare(&[&task.id]);
    let applied = workspace.assess(&prepared, &NARROWED, "medium");
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
    assert_eq!(
        applied["tasks"][0]["context_evidence_retained"],
        json!([DUPLICATES, FILING, CODE_GROUPS]),
        "{applied}"
    );
    assert_eq!(applied["tasks"][0]["context_files_after"], json!(FILED));
    assert_eq!(workspace.scope(&task.id), FILED);

    // Replaying the same apply settles as already applied.
    let replayed = workspace.assess(&prepared, &NARROWED, "medium");
    assert_eq!(
        outcome(&replayed)["outcome"],
        "already_applied",
        "{replayed}"
    );

    // Each re-assessment that narrows again rewrites nothing.
    for _ in 0..2 {
        let prepared = workspace.prepare(&[&task.id]);
        let reassessed = workspace.assess(&prepared, &NARROWED, "medium");
        assert_eq!(outcome(&reassessed)["outcome"], "applied", "{reassessed}");
        assert_eq!(reassessed["tasks"][0]["applied"], false, "{reassessed}");
        assert_eq!(workspace.scope(&task.id), FILED);
    }

    // The pilot can still widen it with a target the evidence did not name.
    let prepared = workspace.prepare(&[&task.id]);
    let widened = workspace.assess(&prepared, &[ADMISSION, EXTRA], "medium");
    assert_eq!(outcome(&widened)["outcome"], "applied", "{widened}");
    assert_eq!(
        workspace.scope(&task.id),
        owned(&[&FILED[..], &[EXTRA]].concat())
    );

    // Only an operator narrows it, and the next pilot keeps that decision.
    workspace
        .runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                context_files: Some(owned(&NARROWED)),
                ..Default::default()
            },
            Some("codex".into()),
            Some("fixture-model".into()),
        )
        .unwrap();
    let prepared = workspace.prepare(&[&task.id]);
    let respected = workspace.assess(&prepared, &[ADMISSION], "medium");
    assert_eq!(outcome(&respected)["outcome"], "applied", "{respected}");
    assert_eq!(workspace.scope(&task.id), NARROWED);
}

#[test]
fn evidence_that_no_longer_resolves_is_dropped_and_reported() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::review_evidence::evidence_that_no_longer_resolves_is_dropped_and_reported",
    ) {
        return;
    }
    let workspace = Workspace::new();
    workspace.commit_files(&FILED);
    let task = workspace.filed_task("duplicate alert", &["code-review"], &FILED);
    workspace.git(&["rm", "-q", CODE_GROUPS.trim_start_matches("file:")]);
    workspace.git(&["commit", "-m", "remove code groups"]);

    // Retention never keeps a selector apply would refuse from the pilot:
    // the deleted source is dropped and named for reauthorization.
    let prepared = workspace.prepare(&[&task.id]);
    let applied = workspace.assess(&prepared, &NARROWED, "medium");
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
    assert_eq!(
        applied["tasks"][0]["context_evidence_retained"],
        json!([DUPLICATES, FILING])
    );
    let findings = applied["tasks"][0]["context_reauthorization_required"]
        .as_array()
        .unwrap();
    assert_eq!(findings.len(), 1, "{applied}");
    assert!(findings[0].as_str().unwrap().contains(CODE_GROUPS));
    assert_eq!(
        workspace.scope(&task.id),
        [DUPLICATES, FILING, ADMISSION, SWEEP_TEST]
    );

    // Proposing the deleted source itself is still refused.
    let prepared = workspace.prepare(&[&task.id]);
    let refused = workspace.assess(&prepared, &[ADMISSION, CODE_GROUPS], "medium");
    assert_eq!(outcome(&refused)["outcome"], "invalid", "{refused}");
}

#[test]
fn a_task_no_review_filed_keeps_the_pilot_narrowing() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::review_evidence::a_task_no_review_filed_keeps_the_pilot_narrowing",
    ) {
        return;
    }
    let workspace = Workspace::new();
    workspace.commit_files(&FILED);
    let task = workspace.filed_task("ordinary", &["friction-curation"], &FILED);

    let prepared = workspace.prepare(&[&task.id]);
    let applied = workspace.assess(&prepared, &NARROWED, "medium");
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
    assert!(
        applied["tasks"][0]
            .get("context_evidence_retained")
            .is_none()
    );
    assert_eq!(workspace.scope(&task.id), NARROWED);
}

#[test]
fn a_swept_proposal_is_applied_with_an_over_attachment_finding() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::review_evidence::a_swept_proposal_is_applied_with_an_over_attachment_finding",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let swept = (0..11)
        .map(|index| format!("file:src/swept/module_{index}.rs"))
        .collect::<Vec<_>>();
    let swept = swept.iter().map(String::as_str).collect::<Vec<_>>();
    workspace.commit_files(&[&FILED[..], &swept].concat());

    // Over the low-tier budget: applied, with the finding attached.
    let ordinary = workspace.filed_task("swept", &[], &[ADMISSION]);
    let prepared = workspace.prepare(&[&ordinary.id]);
    let applied = workspace.assess(&prepared, &swept, "low");
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
    let findings = applied["tasks"][0]["context_attachment_warnings"]
        .as_array()
        .unwrap();
    assert_eq!(findings.len(), 1, "{applied}");
    assert_eq!(workspace.scope(&ordinary.id), owned(&swept));

    // Retained evidence counts toward the budget it reserves.
    let review = workspace.filed_task("review sweep", &["security-review"], &FILED);
    let prepared = workspace.prepare(&[&review.id]);
    let applied = workspace.assess(&prepared, &swept[..6], "low");
    assert_eq!(outcome(&applied)["outcome"], "applied", "{applied}");
    assert_eq!(workspace.scope(&review.id).len(), 11);
    assert_eq!(
        applied["tasks"][0]["context_attachment_warnings"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "{applied}"
    );
}
