use std::fs;

use chrono::Utc;
use orbit_types::task::{TASK_ARTIFACT_SCHEMA_VERSION, TaskCommentRowV2};
use tempfile::tempdir;

use super::super::jsonl::{append_jsonl_row, read_task_comments};

#[test]
fn torn_utf8_comment_tail_is_ignored_on_read_and_removed_before_append() {
    let temp = tempdir().expect("temporary directory");
    let path = temp.path().join("comments.jsonl");
    let first = comment("C-0001", "existing");
    let mut raw = serde_json::to_vec(&first).expect("serialize first comment");
    raw.extend_from_slice(b"\n{\"body\":\"caf\xC3");
    fs::write(&path, raw).expect("write torn comments file");

    let comments = read_task_comments(&path).expect("read before the torn tail");
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].comment_id, first.comment_id);
    assert_eq!(comments[0].body, first.body);

    let second = comment("C-0002", "appended");
    append_jsonl_row(&path, &second).expect("repair torn tail and append");

    let repaired = fs::read(&path).expect("read repaired comments file");
    assert!(repaired.ends_with(b"\n"));
    assert_eq!(repaired.iter().filter(|byte| **byte == b'\n').count(), 2);
    let comments = read_task_comments(&path).expect("read both complete comments");
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[1].comment_id, second.comment_id);
    assert_eq!(comments[1].body, second.body);

    let mut terminated_invalid = serde_json::to_vec(&first).expect("serialize first comment");
    terminated_invalid.extend_from_slice(b"\n{\"body\":\"caf\xC3\n");
    fs::write(&path, terminated_invalid).expect("write terminated invalid UTF-8 row");
    assert!(
        read_task_comments(&path).is_err(),
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
    }
}
