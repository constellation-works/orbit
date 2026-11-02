use super::*;
#[cfg(unix)]
use orbit_common::fs::io::create_dir_symlink;
use orbit_store::contracts::{
    CompatibilityRecord, CompatibilityRefusal, StateComponent, evaluate_newer_state,
};
use orbit_store::workflow::layout::upgrade_workspace_layout;
use std::fs;

#[test]
fn skipped_step_checkpoints_fence_layout_v8_writers() {
    if !support::isolated("layout::skipped_step_checkpoints_fence_layout_v8_writers") {
        return;
    }
    let temp = tempfile::tempdir().expect("workspace layout");
    fs::create_dir_all(temp.path().join("state")).expect("state directory");
    fs::write(temp.path().join("state/layout.version"), "8\n").expect("old layout marker");

    let report = upgrade_workspace_layout(temp.path()).expect("upgrade v8 workspace");
    assert_eq!(report.from_version, 8);
    assert!(
        report.to_version > 8,
        "ORB-21604: writers without skipped_steps must not share the new checkpoint layout"
    );
    let raw = fs::read_to_string(temp.path().join("state/layout.compat"))
        .expect("persisted compatibility record");
    let record = CompatibilityRecord::decode(&raw).expect("decode compatibility record");

    // The v8 pre-flight uses this public contract when the marker is newer
    // than its registry. It must refuse before it can rewrite a checkpoint
    // through the old PipelineState serializer, which drops skipped_steps.
    let refusal = evaluate_newer_state(
        StateComponent::WorkspaceLayout,
        report.to_version,
        8,
        Some(record),
        false,
    )
    .expect_err("a v8 writer must refuse checkpoints carrying skip markers");
    assert!(
        matches!(refusal, CompatibilityRefusal::Breaking(ref entry)
            if entry.version == 9 && entry.name == "skipped-step-checkpoints"),
        "{refusal}"
    );
    let identity = orbit_store::compose::compiled_compatibility().workspace_layout;
    assert_eq!(identity.version, report.to_version);
    assert!(identity.writer_floor >= 9, "live v8 writers must be fenced");
    assert!(
        identity.reader_floor >= 9,
        "v8 readers misread skipped nulls"
    );

    let reopened = upgrade_workspace_layout(temp.path()).expect("new writer reopens");
    assert_eq!(reopened.to_version, report.to_version);
    assert!(reopened.applied.is_empty());
}

#[cfg(unix)]
#[test]
fn legacy_cleanup_does_not_follow_a_symlinked_tasks_parent() {
    if !support::isolated("layout::legacy_cleanup_does_not_follow_a_symlinked_tasks_parent") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().expect("outside tempdir");
    let target = outside.path().join("ORB-00001");
    fs::create_dir(&target).expect("create outside entry");
    create_dir_symlink(outside.path(), &temp.path().join("tasks"))
        .expect("create symlinked parent");

    fs::create_dir_all(temp.path().join("state")).unwrap();
    fs::write(temp.path().join("state/layout.version"), "2\n").unwrap();
    upgrade_workspace_layout(temp.path()).expect("cleanup symlinked parent");

    assert!(temp.path().join("tasks").is_symlink());
    assert!(target.is_dir());
}
