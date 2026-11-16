//! The newest verification entry decides: an unparseable or unsupported
//! newest verdict never revives an older one.

use chrono::Utc;
use orbit_types::task::{TaskHistoryEntry, TaskStatus, TaskType};

use super::{enter_isolated_child, test_runtime};
use crate::application::task::TaskAddParams;
use crate::application::task::finding_verification::{
    FINDING_VERIFICATION_ACTOR, FINDING_VERIFICATION_SCHEMA_VERSION,
};
use crate::application::task::params::TaskRecordUpdateParams;
use crate::application::task::records::TaskRecords;

fn verdict_note(schema_version: u32) -> String {
    serde_json::json!({
        "schema_version": schema_version,
        "verdict": "CONFIRMED",
        "confidence": "high",
        "verified_sha": "a".repeat(40),
        "base_branch": "main",
        "evidence": [],
        "reasoning": "The defect is present.",
        "crews": [],
        "escalated": false,
        "verified_at": Utc::now(),
    })
    .to_string()
}

fn append_verdict(runtime: &crate::OrbitRuntime, task_id: &str, note: String) {
    runtime
        .stores()
        .task_records()
        .update(
            task_id,
            TaskRecordUpdateParams {
                actor: "test".into(),
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: FINDING_VERIFICATION_ACTOR.into(),
                    event: "finding_verified".into(),
                    note: Some(note),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )
        .expect("append verdict");
}

#[test]
fn unparseable_or_unsupported_newest_verdict_revives_no_older_verdict() {
    if !enter_isolated_child(
        module_path!(),
        "unparseable_or_unsupported_newest_verdict_revives_no_older_verdict",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "A review finding".into(),
            description: "The review claims a defect.".into(),
            acceptance_criteria: vec!["The defect is gone.".into()],
            status: Some(TaskStatus::Proposed),
            task_type: Some(TaskType::Bug),
            tags: vec!["code-review".into()],
            ..Default::default()
        })
        .expect("add task");

    append_verdict(
        &runtime,
        &task.id,
        verdict_note(FINDING_VERIFICATION_SCHEMA_VERSION),
    );
    assert!(
        runtime
            .finding_verification(&task.id)
            .expect("read verdict")
            .is_some(),
        "the valid verdict is the latest"
    );

    append_verdict(&runtime, &task.id, "not a verdict record".into());
    assert!(
        runtime
            .finding_verification(&task.id)
            .expect("read verdict")
            .is_none(),
        "an unparseable newest verdict revives no older verdict"
    );

    append_verdict(&runtime, &task.id, verdict_note(99));
    assert!(
        runtime
            .finding_verification(&task.id)
            .expect("read verdict")
            .is_none(),
        "an unsupported schema version revives no older verdict"
    );
}
