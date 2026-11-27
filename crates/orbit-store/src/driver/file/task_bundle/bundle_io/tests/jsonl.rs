//! The JSONL append admission, reached through the bundle store's append
//! boundary. The public task mutations refuse a symlinked bundle directory
//! earlier, at the bundle-lock envelope read, so only a call that skips those
//! outer checks can show the pre-lock path check in `append_jsonl_rows`.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use orbit_types::task::{TASK_ARTIFACT_SCHEMA_VERSION, TASK_COMMENTS_FILE_NAME, TaskCommentRowV2};
use tempfile::TempDir;

use crate::boundary::tests::test_support::bundle_store;

fn listing(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(dir)
        .expect("read dir")
        .map(|entry| {
            let entry = entry.expect("dir entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            (name, fs::read(entry.path()).unwrap_or_default())
        })
        .collect()
}

#[cfg(unix)]
#[test]
fn append_under_symlinked_bundle_dir_leaves_target_directory_untouched() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    let bundle_dir = store.bundle_path("ORB-00000").expect("bundle path");
    let outside_dir = temp.path().join("outside-append-dir");
    fs::create_dir_all(&outside_dir).expect("create outside dir");
    fs::write(outside_dir.join("task.yaml"), b"sentinel\n").expect("seed outside dir");
    fs::create_dir_all(bundle_dir.parent().expect("bundle parent")).expect("create parent");
    std::os::unix::fs::symlink(&outside_dir, &bundle_dir).expect("symlink bundle dir");
    let before = listing(&outside_dir);

    let error = store
        .append_comments(
            "ORB-00000",
            &[TaskCommentRowV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                comment_id: "comment-1".into(),
                at: chrono::Utc::now(),
                by: "test".into(),
                body: "should be refused".into(),
                operation_id: None,
            }],
        )
        .expect_err("append under a symlinked bundle directory must be refused");

    assert!(
        error.to_string().contains("must not be a symlink"),
        "the refusal must come from the bundle path check, got: {error}"
    );
    assert_eq!(
        listing(&outside_dir),
        before,
        "refused append must not create a lock or comments file beside the symlink target"
    );
    assert!(
        !outside_dir.join(TASK_COMMENTS_FILE_NAME).exists(),
        "no comments file may appear in the symlink target"
    );
}
