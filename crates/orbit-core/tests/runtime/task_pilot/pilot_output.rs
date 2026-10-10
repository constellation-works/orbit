//! Formatting slips in a pilot's output that the host can settle itself: the
//! host owns each partition's index, and a missing or scalar `evidence_gaps`
//! is coerced to an array with a note, so neither sends a run down the repair
//! path. Fixtures drive the real prepare and apply actions in isolated
//! children.

use orbit_core::application::task::TaskAddParams;
use orbit_core::{Task, TaskStatus};
use serde_json::{Value, json};

use super::Workspace;

impl Workspace {
    fn pilot_task(&self, title: &str) -> Task {
        self.runtime
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

    fn prepare_partitioned(&self, task_ids: &[&str], max_partition_size: usize) -> Value {
        self.action(
            "prepare_task_pilot",
            json!({
                "task_ids": task_ids, "workspace_path": self.repo,
                "base_branch": "main", "max_partition_size": max_partition_size,
            }),
        )
    }

    fn apply_results(&self, prepared: &Value, results: Value) -> Value {
        self.action(
            "apply_task_pilot_results",
            json!({
                "workspace_path": self.repo, "prepared": prepared, "results": results,
            }),
        )
    }
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

fn result(task_id: &str, partition_index: Option<u64>) -> Value {
    let mut result = json!({"task_ids": [task_id], "tasks": [assessment(task_id)]});
    if let Some(index) = partition_index {
        result["partition_index"] = json!(index);
    }
    result
}

#[test]
fn host_index_applies_to_a_missing_or_wrong_partition_index() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::pilot_output::host_index_applies_to_a_missing_or_wrong_partition_index",
    ) {
        return;
    }
    let workspace = Workspace::new();

    // A single-partition run settles with the host's index whether the pilot
    // omitted its echo or sent another partition's.
    for echoed in [None, Some(7)] {
        let task = workspace.pilot_task(&format!("single {echoed:?}"));
        let prepared = workspace.prepare_partitioned(&[&task.id], 1);
        let applied = workspace.apply_results(&prepared, json!([result(&task.id, echoed)]));
        assert_eq!(applied["status"], "succeeded", "{echoed:?}: {applied}");
        assert_eq!(applied["applied_count"], 1, "{applied}");
        assert_eq!(applied["repair_count"], 0, "{applied}");
        let decision = &applied["partition_decisions"][0];
        assert_eq!(decision["partition_index"], 0, "{decision}");
        assert_eq!(
            decision["pilot_normalizations"][0]["field"], "partition_index",
            "{decision}"
        );
        assert_eq!(
            workspace.runtime.get_task(&task.id).unwrap().context_files,
            ["file:README.md"]
        );
    }

    // Several partitions match by position: the echoes are ignored, the host's
    // indices are recorded, and an echo that already agrees needs no note.
    let first = workspace.pilot_task("first");
    let second = workspace.pilot_task("second");
    let third = workspace.pilot_task("third");
    let prepared = workspace.prepare_partitioned(&[&first.id, &second.id, &third.id], 1);
    assert_eq!(prepared["partition_count"], 3, "{prepared}");
    let applied = workspace.apply_results(
        &prepared,
        json!([
            result(&first.id, Some(0)),
            result(&second.id, None),
            result(&third.id, Some(0)),
        ]),
    );
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(applied["applied_count"], 3, "{applied}");
    let decisions = applied["partition_decisions"].as_array().unwrap();
    let indices = decisions
        .iter()
        .map(|decision| decision["partition_index"].clone())
        .collect::<Vec<_>>();
    assert_eq!(indices, [json!(0), json!(1), json!(2)], "{applied}");
    assert!(decisions[0].get("pilot_normalizations").is_none());
    assert_eq!(
        decisions[1]["pilot_normalizations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        decisions[2]["pilot_normalizations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // The host's index does not launder a result for the wrong tasks.
    let fourth = workspace.pilot_task("fourth");
    let fifth = workspace.pilot_task("fifth");
    let prepared = workspace.prepare_partitioned(&[&fourth.id, &fifth.id], 1);
    let swapped = workspace.apply_results(
        &prepared,
        json!([result(&fifth.id, None), result(&fourth.id, None)]),
    );
    assert_eq!(swapped["applied_count"], 0, "{swapped}");
    assert_eq!(swapped["repair_count"], 2, "{swapped}");
}

#[test]
fn missing_or_scalar_evidence_gaps_are_normalised_to_an_array() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::pilot_output::missing_or_scalar_evidence_gaps_are_normalised_to_an_array",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let cases = [
        ("missing", None, json!([])),
        ("null", Some(Value::Null), json!([])),
        ("empty string", Some(json!("")), json!([])),
        (
            "scalar",
            Some(json!("No test covers the boundary.")),
            json!(["No test covers the boundary."]),
        ),
    ];
    for (label, gaps, expected) in cases {
        let task = workspace.pilot_task(label);
        let prepared = workspace.prepare_partitioned(&[&task.id], 1);
        let mut pilot = result(&task.id, Some(0));
        match gaps {
            Some(gaps) => pilot["tasks"][0]["evidence_gaps"] = gaps,
            None => {
                pilot["tasks"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("evidence_gaps");
            }
        }
        // A non-high confidence admits a recorded gap.
        pilot["tasks"][0]["confidence"] = json!("medium");
        let applied = workspace.apply_results(&prepared, json!([pilot]));
        assert_eq!(applied["status"], "succeeded", "{label}: {applied}");
        assert_eq!(applied["repair_count"], 0, "{label}: {applied}");
        let recorded = &applied["tasks"][0];
        assert_eq!(recorded["evidence_gaps"], expected, "{label}: {recorded}");
        assert_eq!(
            recorded["pilot_normalizations"][0]["field"], "evidence_gaps",
            "{label}: {recorded}"
        );
    }

    // An already-conforming array carries no note, and a shape the host will
    // not guess at is still refused for repair.
    let task = workspace.pilot_task("conforming");
    let prepared = workspace.prepare_partitioned(&[&task.id], 1);
    let applied = workspace.apply_results(&prepared, json!([result(&task.id, Some(0))]));
    assert!(applied["tasks"][0].get("pilot_normalizations").is_none());

    let task = workspace.pilot_task("object");
    let prepared = workspace.prepare_partitioned(&[&task.id], 1);
    let mut pilot = result(&task.id, Some(0));
    pilot["tasks"][0]["evidence_gaps"] = json!({"gap": "x"});
    let refused = workspace.apply_results(&prepared, json!([pilot]));
    assert_eq!(refused["repair_count"], 1, "{refused}");

    // Unassessed complexity still needs actionable gaps, so a missing field
    // is not a way around that rule.
    let task = workspace.pilot_task("unassessed");
    let prepared = workspace.prepare_partitioned(&[&task.id], 1);
    let mut pilot = result(&task.id, Some(0));
    pilot["tasks"][0]["recommended_complexity"] = json!("unassessed");
    pilot["tasks"][0]
        .as_object_mut()
        .unwrap()
        .remove("evidence_gaps");
    let refused = workspace.apply_results(&prepared, json!([pilot]));
    assert_eq!(refused["repair_count"], 1, "{refused}");
}
