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

#[cfg(unix)]
#[test]
fn symlinked_bundle_files_and_directory_are_refused_without_touching_targets() {
    if !support::isolated(
        "jsonl::symlinked_bundle_files_and_directory_are_refused_without_touching_targets",
    ) {
        return;
    }
    let temp = tempdir().expect("temporary directory");
    let store = store(&temp);

    let create_with_outside = |title: &str| {
        let id = store
            .task
            .create_task(create_params(title, TaskStatus::Backlog))
            .unwrap()
            .id;
        let dir = bundle_path(&id);
        (id, dir)
    };

    let comment_update = || TaskHistoryUpdateParams {
        actor: "test".into(),
        append_comments: vec![TaskComment {
            at: Utc::now(),
            by: "test".into(),
            message: "should fail".into(),
            operation_id: None,
        }],
        ..Default::default()
    };

    // 1. Symlinked comments.jsonl (both dangling and existing outside target)
    let (comments_id, comments_bundle) = create_with_outside("Symlinked comments");
    let comments_file = comments_bundle.join(orbit_types::task::TASK_COMMENTS_FILE_NAME);
    let outside_comments = temp.path().join("outside-comments.jsonl");
    fs::remove_file(&comments_file).unwrap();
    std::os::unix::fs::symlink(&outside_comments, &comments_file).unwrap();
    assert!(
        store
            .history
            .update_task_history(&comments_id, comment_update())
            .is_err(),
        "dangling symlinked comments.jsonl must be refused"
    );
    assert!(
        !outside_comments.exists(),
        "dangling symlink target must not be created"
    );
    let sentinel = b"unrelated outside bytes\n";
    fs::write(&outside_comments, sentinel).unwrap();
    assert!(
        store
            .history
            .update_task_history(&comments_id, comment_update())
            .is_err(),
        "symlinked comments.jsonl must be refused"
    );
    assert_eq!(fs::read(&outside_comments).unwrap(), sentinel);

    // 2. Symlinked .pending-write.yaml
    let (pending_id, pending_bundle) = create_with_outside("Symlinked pending");
    let pending_file = pending_bundle.join(".pending-write.yaml");
    let outside_pending = temp.path().join("outside-pending.yaml");
    fs::write(&outside_pending, sentinel).unwrap();
    std::os::unix::fs::symlink(&outside_pending, &pending_file).unwrap();
    assert!(
        store
            .history
            .update_task_history(&pending_id, comment_update())
            .is_err(),
        "symlinked .pending-write.yaml must be refused"
    );
    assert_eq!(fs::read(&outside_pending).unwrap(), sentinel);

    // 3. Symlinked description.md
    let (desc_id, desc_bundle) = create_with_outside("Symlinked description");
    let desc_file = desc_bundle.join(orbit_types::task::TASK_DESCRIPTION_FILE_NAME);
    let outside_desc = temp.path().join("outside-desc.md");
    fs::write(&outside_desc, "secret outside text").unwrap();
    fs::remove_file(&desc_file).unwrap();
    std::os::unix::fs::symlink(&outside_desc, &desc_file).unwrap();
    assert!(
        store.task.get_task(&desc_id).is_err(),
        "symlinked description.md must be refused"
    );

    // 4. Symlinked task.yaml pointing to a valid envelope outside the bundle
    let (env_id, env_bundle) = create_with_outside("Symlinked envelope");
    let env_file = env_bundle.join(orbit_types::task::TASK_ENVELOPE_FILE_NAME);
    let outside_env = temp.path().join("outside-task.yaml");
    fs::copy(&env_file, &outside_env).unwrap();
    fs::remove_file(&env_file).unwrap();
    std::os::unix::fs::symlink(&outside_env, &env_file).unwrap();
    assert!(
        store.task.get_task(&env_id).is_err(),
        "symlinked task.yaml must be refused"
    );

    // 5. Symlinked bundle directory itself
    let (dir_id, dir_bundle) = create_with_outside("Symlinked bundle dir");
    let outside_dir = temp.path().join("outside-bundle-dir");
    fs::create_dir_all(&outside_dir).unwrap();
    for entry in fs::read_dir(&dir_bundle).unwrap() {
        let entry = entry.unwrap();
        let target = outside_dir.join(entry.file_name());
        if entry.file_type().unwrap().is_file() {
            fs::copy(entry.path(), target).unwrap();
        }
    }
    fs::remove_dir_all(&dir_bundle).unwrap();
    std::os::unix::fs::symlink(&outside_dir, &dir_bundle).unwrap();
    assert!(
        store.task.get_task(&dir_id).is_err(),
        "symlinked bundle directory must be refused"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_bundle_dir_append_leaves_outside_directory_untouched() {
    if !support::isolated("jsonl::symlinked_bundle_dir_append_leaves_outside_directory_untouched") {
        return;
    }
    let temp = tempdir().expect("temporary directory");
    let store = store(&temp);
    let id = store
        .task
        .create_task(create_params("Symlinked dir append", TaskStatus::Backlog))
        .unwrap()
        .id;
    let bundle = bundle_path(&id);
    let outside_dir = temp.path().join("outside-append-dir");
    fs::create_dir_all(&outside_dir).unwrap();
    for entry in fs::read_dir(&bundle).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            fs::copy(entry.path(), outside_dir.join(entry.file_name())).unwrap();
        }
    }
    fs::remove_dir_all(&bundle).unwrap();
    std::os::unix::fs::symlink(&outside_dir, &bundle).unwrap();
    let listing = |dir: &std::path::Path| -> std::collections::BTreeMap<String, Vec<u8>> {
        fs::read_dir(dir)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let name = entry.file_name().to_string_lossy().into_owned();
                (name, fs::read(entry.path()).unwrap_or_default())
            })
            .collect()
    };
    let before = listing(&outside_dir);
    assert!(
        store
            .history
            .update_task_history(
                &id,
                TaskHistoryUpdateParams {
                    actor: "test".into(),
                    append_comments: vec![TaskComment {
                        at: Utc::now(),
                        by: "test".into(),
                        message: "should fail".into(),
                        operation_id: None,
                    }],
                    ..Default::default()
                },
            )
            .is_err(),
        "append under a symlinked bundle directory must be refused"
    );
    assert_eq!(
        listing(&outside_dir),
        before,
        "refused append must not create or change files beside the symlink target"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_store_ancestor_keeps_bundle_reads_and_writes_working() {
    if !support::isolated("jsonl::symlinked_store_ancestor_keeps_bundle_reads_and_writes_working") {
        return;
    }
    let temp = tempdir().expect("temporary directory");
    let real_home = temp.path().join("real-home");
    let alias_home = temp.path().join("alias-home");
    fs::create_dir_all(&real_home).unwrap();
    std::os::unix::fs::symlink(&real_home, &alias_home).unwrap();

    let registry = TaskRegistryStore::open(&task_registry_path(&alias_home)).unwrap();
    let backends =
        repair_gate::bound_store_at(&registry, temp.path(), "orbit-alias-123456", "alias-repo");
    let created = backends
        .task
        .create_task(create_params("Alias ancestor", TaskStatus::Backlog))
        .unwrap();
    let initial_comments = backends
        .history
        .get_task_comments(&created.id)
        .expect("read initial comments beneath symlinked ancestor")
        .expect("comments present")
        .len();
    backends
        .history
        .update_task_history(
            &created.id,
            TaskHistoryUpdateParams {
                actor: "test".into(),
                append_comments: vec![TaskComment {
                    at: Utc::now(),
                    by: "test".into(),
                    message: "comment through symlinked ancestor".into(),
                    operation_id: None,
                }],
                ..Default::default()
            },
        )
        .expect("append comment beneath symlinked ancestor");
    let fetched = backends
        .task
        .get_task(&created.id)
        .expect("read task beneath symlinked ancestor")
        .expect("task present");
    assert_eq!(fetched.id, created.id);
    let comments = backends
        .history
        .get_task_comments(&created.id)
        .expect("read comments beneath symlinked ancestor")
        .expect("comments present");
    assert_eq!(comments.len(), initial_comments + 1);
    assert_eq!(
        comments.last().unwrap().message,
        "comment through symlinked ancestor"
    );
}
