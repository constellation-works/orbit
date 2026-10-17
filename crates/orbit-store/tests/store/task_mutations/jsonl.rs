use std::fs;

use chrono::Utc;
use orbit_types::task::{TASK_ARTIFACT_SCHEMA_VERSION, TaskCommentRowV2};
use tempfile::tempdir;

use super::*;

#[test]
fn torn_utf8_comment_tail_is_ignored_on_read_and_removed_before_append() {
    if !support::isolated(
        "jsonl::torn_utf8_comment_tail_is_ignored_on_read_and_removed_before_append",
    ) {
        return;
    }
    let temp = tempdir().expect("temporary directory");
    let store = store(&temp);
    let id = store
        .task
        .create_task(create_params("Torn comments", TaskStatus::Backlog))
        .unwrap()
        .id;
    let path = bundle_path(&id).join(orbit_types::task::TASK_COMMENTS_FILE_NAME);
    let first = comment("C-0001", "existing");
    let mut raw = serde_json::to_vec(&first).expect("serialize first comment");
    raw.extend_from_slice(b"\n{\"body\":\"caf\xC3");
    fs::write(&path, raw).expect("write torn comments file");

    let comments = store
        .history
        .get_task_comments(&id)
        .map(|rows| rows.unwrap())
        .expect("read before the torn tail");
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].message, first.body);
    let recorded_first: TaskCommentRowV2 = serde_json::from_slice(
        fs::read(&path)
            .unwrap()
            .split(|byte| *byte == b'\n')
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(recorded_first.comment_id, first.comment_id);

    let second = comment("C-0002", "appended");
    store
        .history
        .update_task_history(
            &id,
            TaskHistoryUpdateParams {
                actor: "test".into(),
                append_comments: vec![TaskComment {
                    at: second.at,
                    by: second.by.clone(),
                    message: second.body.clone(),
                    operation_id: None,
                }],
                ..Default::default()
            },
        )
        .expect("repair torn tail and append");

    let repaired = fs::read(&path).expect("read repaired comments file");
    assert!(repaired.ends_with(b"\n"));
    assert_eq!(repaired.iter().filter(|byte| **byte == b'\n').count(), 2);
    let comments = store
        .history
        .get_task_comments(&id)
        .map(|rows| rows.unwrap())
        .expect("read both complete comments");
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[1].message, second.body);
    let recorded_second: TaskCommentRowV2 =
        serde_json::from_slice(repaired.split(|byte| *byte == b'\n').nth(1).unwrap()).unwrap();
    assert_eq!(recorded_second.comment_id, second.comment_id);

    let mut terminated_invalid = serde_json::to_vec(&first).expect("serialize first comment");
    terminated_invalid.extend_from_slice(b"\n{\"body\":\"caf\xC3\n");
    fs::write(&path, terminated_invalid).expect("write terminated invalid UTF-8 row");
    assert!(
        store
            .history
            .get_task_comments(&id)
            .map(|rows| rows.unwrap())
            .is_err(),
        "invalid UTF-8 in a newline-terminated row must still fail"
    );
}

fn comment(comment_id: &str, body: &str) -> TaskCommentRowV2 {
    TaskCommentRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        comment_id: comment_id.to_string(),
        at: Utc::now(),
        by: "test".to_string(),
        body: body.to_string(),
        operation_id: None,
    }
}
