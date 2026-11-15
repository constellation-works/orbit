//! Re-check mode [ORB-21124]: an explicit pilot of a task whose latest
//! assessment holds promotion re-checks that assessment instead of piloting
//! in full. Prepare hands the pilot the prior assessment, its holding
//! findings and a field-level task diff; apply requires a verdict per
//! holding finding, keeps every prior field the pilot did not return,
//! records the mode and prior assessment id, and holds the result to the
//! same revision proof as a full assessment. Each fallback to a full pilot
//! is recorded with its reason.

use orbit_core::application::task::TaskUpdateParams;

use super::*;

const HOLD: &str = "The fixture's utility check must pass first.";

fn assessment(task_id: &str) -> Value {
    json!({
        "task_id": task_id, "context_files_after": ["file:README.md"],
        "disposition": "selectors", "recommended_crew": "fixture",
        "recommended_complexity": "medium", "confidence": "high",
        "assessment_rationale": "README.md contains the affected material.",
        "validation_approach": "Inspect the persisted scope.",
        "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
        "adr_conflicts": [], "surface_warnings": [],
        "utility_warnings": [{"text": HOLD, "holds_promotion": true, "evidence": "README.md:1"}],
        "duplicate_of": null, "already_landed": null,
    })
}

impl Workspace {
    fn recheck_task(&self, title: &str) -> Task {
        self.runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: format!("Update the fixture readme for {title}."),
                acceptance_criteria: vec!["The readme names the fixture.".into()],
                plan: "Inspect README.md.".into(),
                status: Some(TaskStatus::Proposed),
                ..Default::default()
            })
            .unwrap()
    }

    fn prepare_with(&self, task_id: &str, extra: Value) -> Value {
        let mut input = json!({
            "task_ids": [task_id], "workspace_path": self.repo, "base_branch": "main",
        });
        for (key, value) in extra.as_object().unwrap() {
            input[key] = value.clone();
        }
        self.action("prepare_task_pilot", input)
    }

    fn apply_tasks(&self, prepared: &Value, tasks: Vec<Value>) -> Value {
        let ids: Vec<_> = tasks.iter().map(|task| task["task_id"].clone()).collect();
        self.action(
            "apply_task_pilot_results",
            json!({
                "workspace_path": self.repo, "prepared": prepared,
                "results": [{"partition_index": 0, "task_ids": ids, "tasks": tasks}],
            }),
        )
    }

    /// Pilot `task` in full with a holding utility finding.
    fn held(&self, task: &Task) -> Value {
        let prepared = self.prepare_with(&task.id, json!({}));
        let applied = self.apply_tasks(&prepared, vec![assessment(&task.id)]);
        assert_eq!(applied["applied_count"], 1, "{applied}");
        prepared
    }

    /// The newest task-pilot receipt: its operation id and audit.
    fn latest_receipt(&self, task_id: &str) -> (String, Value) {
        let comments = self.runtime.get_task_comments(task_id).unwrap();
        let comment = comments
            .iter()
            .rev()
            .find(|comment| comment.by == "task-pilot")
            .expect("a task-pilot receipt");
        let receipt = comment.receipt().expect("a sealed receipt");
        (
            receipt.operation_id.to_string(),
            super::pilot_audit(&self.runtime, task_id, comment),
        )
    }

    fn update(&self, task_id: &str, params: TaskUpdateParams) {
        self.runtime
            .update_task_as_human(task_id, params, "fixture".into())
            .unwrap();
    }
}

fn verdict(status: &str) -> Value {
    let mut verdict = json!({
        "id": "utility_warnings[0]", "status": status,
        "evidence": format!("README.md:1 shows the check {status}."),
    });
    if status == "changed" {
        verdict["current"] =
            json!({"text": "Only the narrower check remains.", "holds_promotion": true});
    }
    verdict
}

fn recheck_result(task_id: &str, status: &str) -> Value {
    json!({"task_id": task_id, "recheck_findings": [verdict(status)]})
}

fn a_held_assessment_is_rechecked_with_its_findings_and_task_diff() {
    let workspace = Workspace::new();
    let task = workspace.recheck_task("held");
    let first = workspace.held(&task);
    let (prior_id, audit) = workspace.latest_receipt(&task.id);
    assert_eq!(audit["assessment"]["mode"], "full", "{audit}");
    assert_eq!(
        audit["recheck_basis"]["source_revision"], first["source"]["source_revision"],
        "{audit}"
    );

    // The repository moves away from the footprint, and the operator edits
    // the criteria and tags within a bounded change.
    workspace.commit_file("other.txt", "unrelated\n", "unrelated change");
    workspace.update(
        &task.id,
        TaskUpdateParams {
            acceptance_criteria: Some(vec![
                "The readme names the fixture.".into(),
                "The readme links the guide.".into(),
            ]),
            tags: Some(vec!["docs".into()]),
            ..Default::default()
        },
    );
    let prepared = workspace.prepare_with(&task.id, json!({}));
    let snapshot = &prepared["tasks"][0];
    assert_eq!(snapshot["pilot_mode"], "recheck", "{snapshot}");
    assert!(snapshot["pilot_mode_reason"].is_null(), "{snapshot}");
    assert_eq!(snapshot["prior_assessment_id"], prior_id, "{snapshot}");
    let input = &snapshot["recheck"];
    assert_eq!(input["prior_assessment_id"], prior_id, "{input}");
    assert_eq!(
        input["prior_source_revision"], first["source"]["source_revision"],
        "{input}"
    );
    assert_ne!(
        input["prior_source_revision"], prepared["source"]["source_revision"],
        "{input}"
    );
    let finding = &input["holding_findings"][0];
    assert_eq!(finding["id"], "utility_warnings[0]", "{input}");
    assert_eq!(finding["category"], "utility_warnings", "{input}");
    assert_eq!(finding["text"], HOLD, "{input}");
    assert_eq!(finding["evidence"], "README.md:1", "{input}");
    assert_eq!(input["holding_findings"].as_array().unwrap().len(), 1);
    assert_eq!(
        input["prior_assessment"]["recommended_complexity"], "medium",
        "{input}"
    );
    let diff = &input["task_diff"];
    assert_eq!(
        diff["acceptance_criteria"]["added"],
        json!(["The readme links the guide."]),
        "{diff}"
    );
    assert_eq!(diff["acceptance_criteria"]["removed"], json!([]), "{diff}");
    assert_eq!(diff["tags"]["added"], json!(["docs"]), "{diff}");
    assert_eq!(diff["title"]["changed"], false, "{diff}");
    assert_eq!(diff["description"]["changed"], false, "{diff}");
    assert_eq!(diff["context_files"]["added"], json!([]), "{diff}");
    for field in ["dependencies", "relations"] {
        assert!(diff[field]["added"].is_array(), "{field}: {diff}");
    }
    assert_eq!(diff["answered_decisions"], json!([]), "{diff}");
    // The pilot receives the same input through its partition.
    assert_eq!(
        &prepared["partitions"][0]["rechecks"][task.id.as_str()],
        input,
        "{prepared}"
    );
}

fn a_recheck_keeps_untouched_fields_and_records_its_verdicts() {
    let workspace = Workspace::new();
    for (status, kept_warnings) in [
        ("resolved", json!([])),
        (
            "still_holds",
            json!([{"text": HOLD, "holds_promotion": true, "evidence": "README.md:1"}]),
        ),
        (
            "changed",
            json!([{"text": "Only the narrower check remains.", "holds_promotion": true}]),
        ),
    ] {
        let task = workspace.recheck_task(status);
        workspace.held(&task);
        let (prior_id, _) = workspace.latest_receipt(&task.id);
        let prepared = workspace.prepare_with(&task.id, json!({}));
        assert_eq!(prepared["tasks"][0]["pilot_mode"], "recheck", "{prepared}");
        let applied = workspace.apply_tasks(&prepared, vec![recheck_result(&task.id, status)]);
        assert_eq!(applied["applied_count"], 1, "{status}: {applied}");

        let (_, audit) = workspace.latest_receipt(&task.id);
        let recorded = &audit["assessment"];
        assert_eq!(recorded["mode"], "recheck", "{recorded}");
        assert_eq!(recorded["prior_assessment_id"], prior_id, "{recorded}");
        assert_eq!(
            recorded["recheck_findings"][0]["status"], status,
            "{recorded}"
        );
        assert_eq!(
            recorded["utility_warnings"], kept_warnings,
            "{status}: {recorded}"
        );
        assert_eq!(recorded["recommended_complexity"], "medium", "{recorded}");
        assert_eq!(recorded["context_files_after"], json!(["file:README.md"]));
        let kept = recorded["recheck_kept_fields"].as_array().unwrap();
        for field in [
            "recommended_complexity",
            "context_files_after",
            "utility_warnings",
        ] {
            assert!(kept.contains(&json!(field)), "{field}: {recorded}");
        }
        let stored = workspace.runtime.get_task(&task.id).unwrap();
        assert_eq!(stored.context_files, ["file:README.md"]);
        assert_eq!(
            stored.complexity,
            Some(orbit_types::task::TaskComplexity::Medium)
        );
    }

    // A complete assessment without verdicts is the pilot escalating to a
    // full pilot: it applies as a full assessment naming its prior.
    let task = workspace.recheck_task("escalated");
    workspace.held(&task);
    let (prior_id, _) = workspace.latest_receipt(&task.id);
    let prepared = workspace.prepare_with(&task.id, json!({}));
    assert_eq!(prepared["tasks"][0]["pilot_mode"], "recheck", "{prepared}");
    let applied = workspace.apply_tasks(&prepared, vec![assessment(&task.id)]);
    assert_eq!(applied["applied_count"], 1, "{applied}");
    let (_, audit) = workspace.latest_receipt(&task.id);
    assert_eq!(audit["assessment"]["mode"], "full", "{audit}");
    assert_eq!(
        audit["assessment"]["mode_reason"], "recheck_escalated",
        "{audit}"
    );
    assert_eq!(
        audit["assessment"]["prior_assessment_id"], prior_id,
        "{audit}"
    );

    // A field the pilot returns replaces the prior one.
    let task = workspace.recheck_task("re-derived");
    workspace.held(&task);
    let prepared = workspace.prepare_with(&task.id, json!({}));
    let mut result = recheck_result(&task.id, "resolved");
    result["recommended_complexity"] = json!("low");
    let applied = workspace.apply_tasks(&prepared, vec![result]);
    assert_eq!(applied["applied_count"], 1, "{applied}");
    let (_, audit) = workspace.latest_receipt(&task.id);
    assert_eq!(audit["assessment"]["recommended_complexity"], "low");
    assert!(
        !audit["assessment"]["recheck_kept_fields"]
            .as_array()
            .unwrap()
            .contains(&json!("recommended_complexity"))
    );
}

fn a_recheck_without_a_verdict_per_holding_finding_is_sent_to_repair() {
    let workspace = Workspace::new();
    for (label, findings) in [
        ("missing", json!([])),
        (
            "unknown",
            json!([{"id": "surface_warnings[0]", "status": "resolved", "evidence": "x"}]),
        ),
        (
            "bad status",
            json!([{"id": "utility_warnings[0]", "status": "maybe", "evidence": "x"}]),
        ),
        (
            "no evidence",
            json!([{"id": "utility_warnings[0]", "status": "resolved", "evidence": " "}]),
        ),
        (
            "changed without current",
            json!([{"id": "utility_warnings[0]", "status": "changed", "evidence": "x"}]),
        ),
    ] {
        let task = workspace.recheck_task(label);
        workspace.held(&task);
        let prepared = workspace.prepare_with(&task.id, json!({}));
        let refused = workspace.apply_tasks(
            &prepared,
            vec![json!({"task_id": task.id, "recheck_findings": findings})],
        );
        assert_eq!(refused["applied_count"], 0, "{label}: {refused}");
        assert_eq!(refused["repair_count"], 1, "{label}: {refused}");
        // The repair pilot re-checks against the same input.
        assert_eq!(
            refused["repair_partitions"][0]["rechecks"][task.id.as_str()],
            prepared["tasks"][0]["recheck"],
            "{label}: {refused}"
        );
    }
}

fn a_recheck_is_held_to_the_same_revision_proof() {
    let workspace = Workspace::new();
    let task = workspace.recheck_task("proof");
    workspace.held(&task);
    let prepared = workspace.prepare_with(&task.id, json!({}));
    assert_eq!(prepared["tasks"][0]["pilot_mode"], "recheck");
    let apply = |result: Value| {
        workspace
            .runtime
            .run_deterministic(
                "apply_task_pilot_results",
                &json!({}),
                &json!({
                    "workspace_path": workspace.repo, "prepared": prepared,
                    "results": [result],
                }),
                ToolContext::default(),
            )
            .unwrap()
    };
    let tasks = json!([recheck_result(&task.id, "resolved")]);
    // No proof, and a proof of another revision, are refused for repair.
    let first_commit = workspace.git(&["rev-list", "--max-parents=0", "HEAD"]);
    for proof in [
        json!({}),
        json!({"observed_revision": "0".repeat(40), "observed_tree": first_commit.trim()}),
    ] {
        let mut result = json!({"partition_index": 0, "task_ids": [task.id], "tasks": tasks});
        for (key, value) in proof.as_object().unwrap() {
            result[key] = value.clone();
        }
        let refused = apply(result);
        assert_eq!(refused["applied_count"], 0, "{refused}");
    }
    let (_, audit) = workspace.latest_receipt(&task.id);
    assert_eq!(
        audit["assessment"]["mode"], "full",
        "nothing re-checked was written"
    );
}

fn full_reason(workspace: &Workspace, task_id: &str, extra: Value) -> Value {
    let prepared = workspace.prepare_with(task_id, extra);
    let snapshot = &prepared["tasks"][0];
    assert_eq!(snapshot["pilot_mode"], "full", "{snapshot}");
    assert!(snapshot.get("recheck").is_none(), "{snapshot}");
    assert!(
        prepared["partitions"][0].get("rechecks").is_none(),
        "{prepared}"
    );
    snapshot["pilot_mode_reason"].clone()
}

fn each_fallback_runs_a_full_pilot_and_records_why() {
    let workspace = Workspace::new();

    // No prior assessment: nothing to re-check.
    let fresh = workspace.recheck_task("fresh");
    assert_eq!(
        full_reason(&workspace, &fresh.id, json!({})),
        "no_prior_assessment"
    );

    // The operator asked for a full pilot (`repilot --full`).
    let task = workspace.recheck_task("operator full");
    workspace.held(&task);
    assert_eq!(
        full_reason(&workspace, &task.id, json!({"full_pilot": true})),
        "operator_requested_full"
    );

    // The prior assessment is older than the configured age.
    assert_eq!(
        full_reason(&workspace, &task.id, json!({"recheck_max_age_hours": 0})),
        "prior_too_old"
    );

    // Title or description rewritten beyond the bounded threshold.
    let task = workspace.recheck_task("rewritten");
    workspace.held(&task);
    workspace.update(
        &task.id,
        TaskUpdateParams {
            description: Some("Replace the deployment pipeline with a new scheduler.".into()),
            ..Default::default()
        },
    );
    assert_eq!(
        full_reason(&workspace, &task.id, json!({})),
        "scope_rewritten"
    );

    // New files in the task's scope.
    let task = workspace.recheck_task("widened");
    workspace.held(&task);
    workspace.commit_file("guide.md", "guide\n", "add guide");
    workspace.update(
        &task.id,
        TaskUpdateParams {
            context_files: Some(vec!["file:README.md".into(), "file:guide.md".into()]),
            ..Default::default()
        },
    );
    assert_eq!(full_reason(&workspace, &task.id, json!({})), "scope_added");

    // The prior assessment was itself a re-check: no chains.
    let task = workspace.recheck_task("chained");
    workspace.held(&task);
    let prepared = workspace.prepare_with(&task.id, json!({}));
    let applied = workspace.apply_tasks(&prepared, vec![recheck_result(&task.id, "still_holds")]);
    assert_eq!(applied["applied_count"], 1, "{applied}");
    assert_eq!(
        full_reason(&workspace, &task.id, json!({})),
        "prior_was_recheck"
    );
    // That full pilot records the re-check it follows.
    let (recheck_id, _) = workspace.latest_receipt(&task.id);
    let prepared = workspace.prepare_with(&task.id, json!({}));
    let applied = workspace.apply_tasks(&prepared, vec![assessment(&task.id)]);
    assert_eq!(applied["applied_count"], 1, "{applied}");
    let (_, audit) = workspace.latest_receipt(&task.id);
    assert_eq!(audit["assessment"]["mode"], "full", "{audit}");
    assert_eq!(
        audit["assessment"]["prior_assessment_id"], recheck_id,
        "{audit}"
    );
    assert_eq!(
        audit["assessment"]["mode_reason"], "prior_was_recheck",
        "{audit}"
    );

    // A clean prior assessment holds nothing to re-check.
    let task = workspace.recheck_task("clean");
    let prepared = workspace.prepare_with(&task.id, json!({}));
    let mut clean = assessment(&task.id);
    clean["utility_warnings"] = json!([]);
    workspace.apply_tasks(&prepared, vec![clean]);
    assert_eq!(
        full_reason(&workspace, &task.id, json!({})),
        "prior_not_held"
    );

    // The repository moved at the footprint since the prior source revision.
    // Last: it moves every other task's footprint too.
    let task = workspace.recheck_task("moved");
    workspace.held(&task);
    workspace.commit_file("README.md", "fixture, revised\n", "revise readme");
    assert_eq!(
        full_reason(&workspace, &task.id, json!({})),
        "footprint_moved"
    );
}

crate::scenario_table!(
    recheck_scenarios,
    a_held_assessment_is_rechecked_with_its_findings_and_task_diff,
    a_recheck_keeps_untouched_fields_and_records_its_verdicts,
    a_recheck_without_a_verdict_per_holding_finding_is_sent_to_repair,
    a_recheck_is_held_to_the_same_revision_proof,
    each_fallback_runs_a_full_pilot_and_records_why
);
