use super::*;
use crate::members::preparation;

fn fingerprint_task() -> orbit_types::task::Task {
    serde_json::from_value(serde_json::json!({
        "id":"ORB-00001", "title":"task", "description":"scope",
        "context_files":["file:src/lib.rs"], "status":"backlog",
        "priority":"medium", "task_type":"chore", "tags":["pilot"],
        "created_at":"2026-09-01T00:00:00Z", "updated_at":"2026-09-01T00:00:00Z"
    }))
    .unwrap()
}

fn freshness_policy(fields: &[MaterialField], source: SourceSensitivity) -> PreparationPolicy {
    PreparationPolicy {
        freshness: PreparationFreshness {
            material_fields: fields.to_vec(),
            source_sensitivity: source,
        },
        ..Default::default()
    }
}

/// Every task field a policy can name, changed one at a time.
fn field_edits() -> Vec<(MaterialField, serde_json::Value)> {
    use serde_json::json;
    vec![
        (MaterialField::Title, json!({"title": "changed"})),
        (
            MaterialField::Description,
            json!({"description": "changed"}),
        ),
        (
            MaterialField::Criteria,
            json!({"acceptance_criteria": ["changed"]}),
        ),
        (MaterialField::Plan, json!({"plan": "changed"})),
        (
            MaterialField::Selectors,
            json!({"context_files": ["file:src/other.rs"]}),
        ),
        (MaterialField::Tags, json!({"tags": ["pilot", "retagged"]})),
        (MaterialField::Crew, json!({"crew": "opus"})),
        (
            MaterialField::Tools,
            json!({"required_tools": ["orbit.task.show"]}),
        ),
        (MaterialField::Type, json!({"task_type": "feature"})),
        (MaterialField::Complexity, json!({"complexity": "low"})),
        (
            MaterialField::Relations,
            json!({"relations": [{"type": "blocked_by", "target": "ORB-00002"}]}),
        ),
    ]
}

fn edited(task: &orbit_types::task::Task, edit: &serde_json::Value) -> orbit_types::task::Task {
    let mut value = serde_json::to_value(task).unwrap();
    for (key, field) in edit.as_object().unwrap() {
        value[key] = field.clone();
    }
    serde_json::from_value(value).unwrap()
}

/// [ORB-13638] By default only what the assessment is about — title,
/// description, criteria, plan and selectors — is material. Crew, tags,
/// tools, type, complexity, relations, dependency evidence, instructions and
/// the branch head are not, and audit writes never are.
#[test]
fn default_material_is_the_tasks_meaning_and_selectors_only() {
    use orbit_types::task::TaskPriority;
    use serde_json::json;
    let task = fingerprint_task();
    let policy = PreparationPolicy::default();
    let evidence = MaterialEvidence::default();
    let baseline = preparation::fingerprint(&task, &evidence, &policy).unwrap();

    let mut audited = task.clone();
    audited.priority = TaskPriority::High;
    audited.updated_at += Duration::minutes(1);
    audited.execution_summary = "audit write".into();
    assert_eq!(
        baseline,
        preparation::fingerprint(&audited, &evidence, &policy).unwrap()
    );

    for (field, edit) in field_edits() {
        let changed = preparation::fingerprint(&edited(&task, &edit), &evidence, &policy).unwrap();
        if MaterialField::DEFAULT.contains(&field) {
            assert_ne!(baseline, changed, "{field:?} is default material");
        } else {
            assert_eq!(baseline, changed, "{field:?} is not default material");
        }
    }
    let outside = MaterialEvidence {
        source: json!("another-head"),
        dependencies: json!([{"id": "ORB-00002", "status": "done"}]),
        assignment: json!({"effective_assignment": {"crew": "opus"}}),
        instructions: json!("new instructions"),
    };
    assert_eq!(
        baseline,
        preparation::fingerprint(&task, &outside, &policy).unwrap(),
        "evidence outside the default set is not material"
    );
}

/// [ORB-13638] Opting a field in makes exactly that field material; the
/// source mode decides whether the head identity Core supplies is hashed.
#[test]
fn opted_in_fields_and_source_modes_are_material() {
    use serde_json::json;
    let task = fingerprint_task();
    let evidence = MaterialEvidence::default();
    for (field, edit) in field_edits() {
        let policy = freshness_policy(&[field], SourceSensitivity::Ignore);
        assert_ne!(
            preparation::fingerprint(&task, &evidence, &policy).unwrap(),
            preparation::fingerprint(&edited(&task, &edit), &evidence, &policy).unwrap(),
            "{field:?} opted in"
        );
    }
    for (field, changed) in [
        (
            MaterialField::Dependencies,
            MaterialEvidence {
                dependencies: json!([{"id": "ORB-00002", "status": "done"}]),
                ..Default::default()
            },
        ),
        (
            MaterialField::Crew,
            MaterialEvidence {
                assignment: json!({"effective_assignment": {"model": "other"}}),
                ..Default::default()
            },
        ),
        (
            MaterialField::Instructions,
            MaterialEvidence {
                instructions: json!("new instructions"),
                ..Default::default()
            },
        ),
    ] {
        let policy = freshness_policy(&[field], SourceSensitivity::Ignore);
        assert_ne!(
            preparation::fingerprint(&task, &evidence, &policy).unwrap(),
            preparation::fingerprint(&task, &changed, &policy).unwrap(),
            "{field:?} evidence opted in"
        );
    }

    let moved = MaterialEvidence {
        source: json!("another-head"),
        ..Default::default()
    };
    let head = MaterialEvidence {
        source: json!("head"),
        ..Default::default()
    };
    for (mode, material) in [
        (SourceSensitivity::Ignore, false),
        (SourceSensitivity::ContextFiles, true),
        (SourceSensitivity::Any, true),
    ] {
        let policy = freshness_policy(&MaterialField::DEFAULT, mode);
        assert_eq!(
            preparation::fingerprint(&task, &head, &policy).unwrap()
                != preparation::fingerprint(&task, &moved, &policy).unwrap(),
            material,
            "{mode:?}"
        );
    }
}

/// [ORB-12745, ORB-13638] A resolved eligibility or freshness that differs
/// from the default is itself material, so changing either invalidates
/// assessments accepted under the old one; an equivalent block authored in
/// another order hashes the same, and the defaults add nothing.
#[test]
fn material_fingerprint_folds_in_non_default_policy() {
    use orbit_types::task::{TaskStatus, TaskType};
    use serde_json::json;
    let task = fingerprint_task();
    let evidence = MaterialEvidence::default();
    let hash =
        |policy: &PreparationPolicy| preparation::fingerprint(&task, &evidence, policy).unwrap();
    let baseline = hash(&PreparationPolicy::default());
    // The deliberate `material_v2` default bytes.
    let default_material = json!({
        "contract": preparation::CONTRACT, "id": task.id, "eligible": true,
        "title": "task", "description": "scope", "criteria": [], "plan": "",
        "selectors": ["file:src/lib.rs"],
    });
    assert_eq!(
        baseline,
        crate::delivery::definition_epoch(&default_material).unwrap()
    );

    let spelled_out = PreparationPolicy {
        eligibility: PreparationEligibility {
            statuses: vec![TaskStatus::Backlog, TaskStatus::Proposed],
            exclude_tags: vec!["no-diff-needed".into(), "no-diff-expected".into()],
            require_tags: vec![],
            task_types: vec![],
        },
        freshness: PreparationFreshness {
            material_fields: MaterialField::DEFAULT.iter().rev().copied().collect(),
            source_sensitivity: SourceSensitivity::Ignore,
        },
    };
    assert_eq!(
        baseline,
        hash(&spelled_out),
        "an explicit policy equal to the default is the same material"
    );

    let narrowed = PreparationPolicy {
        eligibility: PreparationEligibility {
            require_tags: vec!["pilot".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(preparation::eligible(&task, &narrowed.eligibility));
    assert_ne!(
        baseline,
        hash(&narrowed),
        "a changed predicate is new material"
    );
    let reordered = PreparationPolicy {
        eligibility: PreparationEligibility {
            statuses: vec![TaskStatus::Backlog, TaskStatus::Proposed],
            ..narrowed.eligibility.clone()
        },
        ..Default::default()
    };
    assert_eq!(
        hash(&narrowed),
        hash(&reordered),
        "authoring order is not material"
    );
    let excluding = PreparationPolicy {
        eligibility: PreparationEligibility {
            task_types: vec![TaskType::Feature],
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(!preparation::eligible(&task, &excluding.eligibility));
    assert_ne!(hash(&narrowed), hash(&excluding));

    let mut fields = MaterialField::DEFAULT.to_vec();
    fields.push(MaterialField::Crew);
    assert_ne!(
        baseline,
        hash(&freshness_policy(&fields, SourceSensitivity::Ignore)),
        "a widened material set is new material even when the new field is unset"
    );
    assert_ne!(
        baseline,
        hash(&freshness_policy(
            &MaterialField::DEFAULT,
            SourceSensitivity::Any
        )),
        "a changed source mode is new material"
    );
}

/// [ORB-13638] The legacy hash stays the exact `material_v1` bytes accepted
/// assessments certified, so Core can recognise one it may carry forward.
#[test]
fn legacy_fingerprint_keeps_the_material_v1_bytes() {
    use serde_json::json;
    let task = fingerprint_task();
    let legacy = preparation::legacy_fingerprint(
        &task,
        "source",
        &json!({}),
        "instructions",
        &PreparationEligibility::default(),
    )
    .unwrap();
    let material_v1 = json!({
        "contract": "material_v1", "id": task.id, "title": task.title.trim(),
        "description": task.description.trim(), "criteria": task.acceptance_criteria,
        "plan": task.plan.trim(), "selectors": task.context_files, "tags": task.tags,
        "tools": task.required_tools, "type": task.task_type, "complexity": task.complexity,
        "crew": task.crew, "eligible": true, "relations": task.relations,
        "dependencies": json!({}), "instructions": "instructions",
        "source_revision": "source",
    });
    assert_eq!(
        legacy,
        crate::delivery::definition_epoch(&material_v1).unwrap()
    );
}
