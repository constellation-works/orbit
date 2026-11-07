//! A pilot assessment that judges a review-filed bug invalid rejects it with
//! the pilot's evidence; the same judgement on any other task, or without
//! evidence, is refused before any write. Fixtures drive the real prepare and
//! apply actions against a real store and repository in isolated children.

use orbit_core::application::task::{
    FindingVerdict, TaskAddParams, TaskUpdateParams, VerdictSource,
};
use orbit_core::{Task, TaskStatus, TaskType};
use orbit_engine::RuntimeHost;
use orbit_types::workflow::handoff::{FindingInvalidCitation, FindingInvalidReport};
use serde_json::{Value, json};

use super::Workspace;

const EVIDENCE: &str =
    "README.md:1 holds only the fixture line; nothing reads it as the review claims.";

impl Workspace {
    fn finding(&self, tags: &[&str]) -> Task {
        self.runtime
            .add_task(TaskAddParams {
                title: "README is parsed twice".into(),
                description: "The review claims README.md is parsed twice.".into(),
                acceptance_criteria: vec!["README.md is parsed once.".into()],
                plan: "Inspect the cited source.".into(),
                status: Some(TaskStatus::Proposed),
                task_type: Some(TaskType::Bug),
                tags: tags.iter().map(|tag| tag.to_string()).collect(),
                context_files: vec!["file:README.md".into()],
                ..Default::default()
            })
            .unwrap()
    }

    fn judge(&self, prepared: &Value, finding_invalid: Value) -> Value {
        let task_id = prepared["task_ids"][0].clone();
        let assessment = if finding_invalid.is_null() {
            json!({ "context_files_after": ["file:README.md"], "disposition": "selectors" })
        } else {
            json!({
                "context_files_after": [], "disposition": "verified_no_diff",
                "evidence": "The defect the review describes does not exist.",
            })
        };
        let mut task = json!({
            "task_id": task_id, "context_evidence_dropped": [],
            "recommended_crew": "fixture", "recommended_complexity": "low",
            "confidence": "high",
            "assessment_rationale": "The cited source was read at the pinned revision.",
            "validation_approach": "Inspect the persisted verdict.",
            "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
            "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
            "duplicate_of": null, "already_landed": null, "finding_invalid": finding_invalid,
        });
        task.as_object_mut()
            .unwrap()
            .extend(assessment.as_object().unwrap().clone());
        self.action(
            "apply_task_pilot_results",
            json!({
                "workspace_path": self.repo,
                "prepared": prepared,
                "results": [{ "partition_index": 0, "task_ids": [task_id], "tasks": [task] }],
            }),
        )
    }
}

fn invalid_review_finding_is_rejected_with_the_pilot_evidence() {
    let workspace = Workspace::new();
    let task = workspace.finding(&["code-review"]);
    let prepared = workspace.prepare(&[&task.id]);
    let applied = workspace.judge(&prepared, json!({ "evidence": EVIDENCE }));
    let outcome = &applied["task_outcomes"][0];
    assert_eq!(outcome["outcome"], "applied", "{applied}");
    assert_eq!(
        applied["tasks"][0]["finding_rejection"]["recorded"], true,
        "{applied}"
    );

    let rejected = workspace.runtime.get_task(&task.id).unwrap();
    assert_eq!(rejected.status, TaskStatus::Rejected);
    let verdict = workspace
        .runtime
        .finding_verification(&task.id)
        .unwrap()
        .expect("a recorded verdict");
    assert_eq!(verdict.verdict, FindingVerdict::Rejected);
    assert_eq!(verdict.source, VerdictSource::TaskPilot);
    assert_eq!(verdict.verified_sha, prepared["source"]["source_revision"]);
    assert_eq!(verdict.reasoning, EVIDENCE);
    assert_eq!(verdict.rejection.unwrap().detail, EVIDENCE);
}

fn finding_invalid_without_a_review_finding_or_evidence_writes_nothing() {
    for (tags, finding_invalid) in [
        (&[][..], json!({ "evidence": EVIDENCE })),
        (&["code-review"][..], json!({ "evidence": "  " })),
    ] {
        let workspace = Workspace::new();
        let task = workspace.finding(tags);
        let prepared = workspace.prepare(&[&task.id]);
        let history = workspace.runtime.get_task_history(&task.id).unwrap().len();
        let applied = workspace.judge(&prepared, finding_invalid);
        assert_eq!(
            applied["task_outcomes"][0]["outcome"], "invalid",
            "{applied}"
        );
        assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), task);
        assert_eq!(
            workspace.runtime.get_task_history(&task.id).unwrap().len(),
            history,
            "{tags:?}: {applied}"
        );
        assert!(
            workspace
                .runtime
                .finding_verification(&task.id)
                .unwrap()
                .is_none()
        );
    }
}

fn null_finding_invalid_applies_as_before() {
    let workspace = Workspace::new();
    let task = workspace.finding(&["code-review"]);
    let prepared = workspace.prepare(&[&task.id]);
    let applied = workspace.judge(&prepared, Value::Null);
    assert_eq!(
        applied["task_outcomes"][0]["outcome"], "applied",
        "{applied}"
    );
    assert!(applied["tasks"][0].get("finding_rejection").is_none());
    assert_eq!(
        workspace.runtime.get_task(&task.id).unwrap().status,
        TaskStatus::Proposed
    );
    assert!(
        workspace
            .runtime
            .finding_verification(&task.id)
            .unwrap()
            .is_none()
    );
}

impl Workspace {
    /// The report an implementer writes after judging the prompt it was given.
    fn implementer_report(&self, task: &Task) -> FindingInvalidReport {
        let prompt = RuntimeHost::task_context_for_agent_input(
            &self.runtime,
            "jrun-fixture",
            "implement",
            &json!({ "task_id": task.id }),
        )
        .unwrap()
        .expect("a task context");
        FindingInvalidReport {
            schema_version: 1,
            task_id: task.id.clone(),
            run_id: "jrun-fixture".into(),
            tested_head: "a".repeat(40),
            finding_fingerprint: prompt["finding_fingerprint"]
                .as_str()
                .expect("a review finding's prompt carries its fingerprint")
                .to_string(),
            reasoning: EVIDENCE.into(),
            evidence: vec![FindingInvalidCitation {
                location: "README.md:1".into(),
                note: "The only line.".into(),
            }],
        }
    }

    fn set_status(&self, task_id: &str, status: TaskStatus) {
        self.runtime
            .force_update_task_with_identity(
                task_id,
                TaskUpdateParams {
                    status: Some(status),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
    }

    fn start(&self, task: &Task) {
        self.set_status(&task.id, TaskStatus::InProgress);
    }
}

fn implementer_rejection_binds_the_material_its_prompt_carried() {
    let workspace = Workspace::new();
    let task = workspace.finding(&["code-review"]);
    workspace.start(&task);
    let report = workspace.implementer_report(&task);

    // An edit after the prompt: the report judged a different claim.
    workspace
        .runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                description: Some("The review now claims README.md is empty.".into()),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    let history = workspace.runtime.get_task_history(&task.id).unwrap();
    let error = RuntimeHost::reject_invalid_finding(&workspace.runtime, &report, "main")
        .expect_err("a stale report refuses");
    assert!(error.to_string().contains("material_changed"), "{error}");
    assert_eq!(
        workspace.runtime.get_task(&task.id).unwrap().status,
        TaskStatus::InProgress
    );
    assert_eq!(
        workspace.runtime.get_task_history(&task.id).unwrap(),
        history
    );

    // A report for the current prompt rejects, and replays without a write.
    let report = workspace.implementer_report(&workspace.runtime.get_task(&task.id).unwrap());
    RuntimeHost::reject_invalid_finding(&workspace.runtime, &report, "main").unwrap();
    assert_eq!(
        workspace.runtime.get_task(&task.id).unwrap().status,
        TaskStatus::Rejected
    );
    let verdict = workspace
        .runtime
        .finding_verification(&task.id)
        .unwrap()
        .unwrap();
    assert_eq!(verdict.source, VerdictSource::Implementer);
    assert_eq!(
        verdict.finding_fingerprint.as_deref(),
        Some(report.finding_fingerprint.as_str())
    );
    let history = workspace.runtime.get_task_history(&task.id).unwrap();
    RuntimeHost::reject_invalid_finding(&workspace.runtime, &report, "main").unwrap();
    assert_eq!(
        workspace.runtime.get_task_history(&task.id).unwrap(),
        history
    );
}

fn implementer_rejection_never_reaches_a_task_outside_its_run() {
    for status in [TaskStatus::Backlog, TaskStatus::Archived, TaskStatus::Done] {
        let workspace = Workspace::new();
        let task = workspace.finding(&["code-review"]);
        workspace.start(&task);
        let report = workspace.implementer_report(&task);
        workspace.set_status(&task.id, status);
        let history = workspace.runtime.get_task_history(&task.id).unwrap();
        let error = RuntimeHost::reject_invalid_finding(&workspace.runtime, &report, "main")
            .expect_err("a task outside its run refuses");
        assert!(
            error.to_string().contains("status_changed"),
            "{status}: {error}"
        );
        assert_eq!(workspace.runtime.get_task(&task.id).unwrap().status, status);
        assert_eq!(
            workspace.runtime.get_task_history(&task.id).unwrap(),
            history
        );
        assert!(
            workspace
                .runtime
                .finding_verification(&task.id)
                .unwrap()
                .is_none()
        );
    }
}

crate::scenario_table!(
    finding_invalid_scenarios,
    invalid_review_finding_is_rejected_with_the_pilot_evidence,
    finding_invalid_without_a_review_finding_or_evidence_writes_nothing,
    null_finding_invalid_applies_as_before,
    implementer_rejection_binds_the_material_its_prompt_carried,
    implementer_rejection_never_reaches_a_task_outside_its_run,
);
