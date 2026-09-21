//! The configurable preparation predicate [ORB-12745]: schema round-trip,
//! defaults equal to the previously hard-coded rule, validation, and
//! evaluation.

use crate::task::{Task, TaskStatus, TaskType};
use crate::workflow::automation::members::{
    PreparationEligibility, StateTrigger, StateTriggerKind,
};
use serde_json::json;

fn trigger(kind: StateTriggerKind, eligibility: PreparationEligibility) -> StateTrigger {
    StateTrigger {
        kind,
        owner_machine: "hm_test".into(),
        branch: "main".into(),
        debounce_minutes: 2,
        max_wait_minutes: 10,
        max_items: 50,
        retries: 1,
        deadline_minutes: 90,
        batch_size: None,
        eligibility,
    }
}

fn task(status: &str, task_type: &str, tags: &[&str]) -> Task {
    serde_json::from_value(json!({
        "id": "ORB-1", "title": "task", "description": "scope", "status": status,
        "context_files": [],
        "priority": "medium", "task_type": task_type, "tags": tags,
        "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z"
    }))
    .expect("task fixture")
}

/// A definition without the block, and one spelling out the defaults,
/// resolve to the same predicate; every key is optional and unknown keys
/// are rejected.
#[test]
fn eligibility_block_is_optional_defaulted_and_closed() {
    let without: StateTrigger = serde_json::from_value(json!({
        "kind": "preparation_eligible", "owner_machine": "hm_test", "branch": "main",
        "debounce_minutes": 2, "max_wait_minutes": 10, "max_items": 50,
        "retries": 1, "deadline_minutes": 90
    }))
    .expect("a definition without eligibility still parses");
    assert!(without.eligibility.is_default());
    assert_eq!(
        without.eligibility,
        PreparationEligibility {
            statuses: vec![TaskStatus::Proposed, TaskStatus::Backlog],
            exclude_tags: vec!["no-diff-expected".into(), "no-diff-needed".into()],
            require_tags: vec![],
            task_types: vec![],
        }
    );

    let spelled: StateTrigger = serde_json::from_value(json!({
        "kind": "preparation_eligible", "owner_machine": "hm_test", "branch": "main",
        "debounce_minutes": 2, "max_wait_minutes": 10, "max_items": 50,
        "retries": 1, "deadline_minutes": 90,
        "eligibility": {
            "statuses": ["proposed", "backlog"],
            "exclude_tags": ["no-diff-expected", "no-diff-needed"],
            "require_tags": [], "task_types": []
        }
    }))
    .expect("spelled-out defaults parse");
    assert_eq!(spelled, without);

    let partial: PreparationEligibility =
        serde_json::from_value(json!({"statuses": ["backlog"]})).expect("partial block parses");
    assert_eq!(partial.statuses, vec![TaskStatus::Backlog]);
    assert_eq!(
        partial.exclude_tags,
        PreparationEligibility::default().exclude_tags
    );

    let unknown = serde_json::from_value::<PreparationEligibility>(json!({"status": ["backlog"]}));
    assert!(unknown.is_err(), "unknown keys must be rejected");

    let round_trip: PreparationEligibility = serde_json::from_value(
        serde_json::to_value(PreparationEligibility {
            statuses: vec![TaskStatus::Backlog],
            exclude_tags: vec!["skip".into()],
            require_tags: vec!["pilot".into()],
            task_types: vec![TaskType::Bug],
        })
        .expect("serialize"),
    )
    .expect("deserialize");
    assert_eq!(round_trip.task_types, vec![TaskType::Bug]);
}

#[test]
fn eligibility_validation_rejects_unusable_predicates() {
    let valid = |eligibility: PreparationEligibility| {
        trigger(StateTriggerKind::PreparationEligible, eligibility).validate()
    };
    assert!(valid(PreparationEligibility::default()).is_ok());
    assert!(
        valid(PreparationEligibility {
            statuses: vec![],
            ..Default::default()
        })
        .is_err(),
        "no status admits nothing"
    );
    assert!(
        valid(PreparationEligibility {
            statuses: vec![TaskStatus::InProgress],
            ..Default::default()
        })
        .is_err(),
        "started work carries no preparation material"
    );
    assert!(
        valid(PreparationEligibility {
            exclude_tags: vec![" ".into()],
            ..Default::default()
        })
        .is_err()
    );
    assert!(
        valid(PreparationEligibility {
            require_tags: vec!["no-diff-needed".into()],
            ..Default::default()
        })
        .is_err(),
        "a tag cannot be both required and excluded"
    );
    // Retained execution_failed definitions never carried a predicate.
    assert!(
        trigger(
            StateTriggerKind::ExecutionFailed,
            PreparationEligibility::default()
        )
        .validate()
        .is_ok()
    );
    assert!(
        trigger(
            StateTriggerKind::ExecutionFailed,
            PreparationEligibility {
                statuses: vec![TaskStatus::Backlog],
                ..Default::default()
            }
        )
        .validate()
        .is_err()
    );
}

#[test]
fn eligibility_admits_by_status_type_and_tags() {
    let default = PreparationEligibility::default();
    assert!(default.admits(&task("proposed", "feature", &[])));
    assert!(default.admits(&task("backlog", "bug", &["docs"])));
    assert!(!default.admits(&task("in_progress", "feature", &[])));
    assert!(!default.admits(&task("backlog", "feature", &["no-diff-expected"])));
    assert!(!default.admits(&task("proposed", "feature", &["x", "no-diff-needed"])));

    let narrowed = PreparationEligibility {
        statuses: vec![TaskStatus::Backlog],
        exclude_tags: vec!["manual".into()],
        require_tags: vec!["pilot".into()],
        task_types: vec![TaskType::Bug, TaskType::Chore],
    };
    assert!(narrowed.admits(&task("backlog", "bug", &["pilot"])));
    assert!(
        !narrowed.admits(&task("proposed", "bug", &["pilot"])),
        "status"
    );
    assert!(
        !narrowed.admits(&task("backlog", "feature", &["pilot"])),
        "type"
    );
    assert!(
        !narrowed.admits(&task("backlog", "bug", &[])),
        "required tag"
    );
    assert!(
        !narrowed.admits(&task("backlog", "bug", &["pilot", "manual"])),
        "excluded tag"
    );
    // The previously excluded no-diff tags are ordinary tags once replaced.
    assert!(narrowed.admits(&task("backlog", "chore", &["pilot", "no-diff-needed"])));
}

#[test]
fn eligibility_normalizes_order_and_duplicates() {
    let authored = PreparationEligibility {
        statuses: vec![
            TaskStatus::Backlog,
            TaskStatus::Proposed,
            TaskStatus::Backlog,
        ],
        exclude_tags: vec!["b".into(), "a".into(), "a".into()],
        require_tags: vec!["z".into(), "y".into()],
        task_types: vec![TaskType::Chore, TaskType::Bug],
    };
    let normalized = authored.normalized();
    assert_eq!(normalized, normalized.normalized());
    assert_eq!(normalized.exclude_tags, vec!["a", "b"]);
    assert_eq!(normalized.require_tags, vec!["y", "z"]);
    assert_eq!(normalized.task_types, vec![TaskType::Bug, TaskType::Chore]);
    assert_eq!(normalized.statuses.len(), 2);
    let reordered = PreparationEligibility {
        statuses: vec![TaskStatus::Proposed, TaskStatus::Backlog],
        exclude_tags: vec!["a".into(), "b".into()],
        require_tags: vec!["y".into(), "z".into()],
        task_types: vec![TaskType::Bug, TaskType::Chore],
    };
    assert_eq!(normalized, reordered.normalized());
    assert!(!normalized.is_default());
    assert!(
        PreparationEligibility {
            statuses: vec![TaskStatus::Backlog, TaskStatus::Proposed],
            exclude_tags: vec!["no-diff-needed".into(), "no-diff-expected".into()],
            ..Default::default()
        }
        .is_default(),
        "the default predicate is recognised in any authored order"
    );
}
