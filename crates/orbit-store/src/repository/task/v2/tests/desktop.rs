use super::*;
use crate::contracts::DesktopTaskMutationParams;
use crate::driver::file::task_bundle::{BundleWriteFault, inject_bundle_write_faults};

fn isolated(test: &str) -> bool {
    let module = module_path!()
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(module_path!());
    let name = format!("{module}::{test}");
    if std::env::var("ORBIT_TEST_DESKTOP_STORE_CHILD")
        .ok()
        .as_deref()
        == Some(&name)
    {
        return true;
    }
    let home = TempDir::new().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let result = command
        .args(["--exact", &name, "--nocapture", "--test-threads=1"])
        .env("ORBIT_TEST_DESKTOP_STORE_CHILD", &name)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "isolated desktop store fixture failed: {}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("test result: ok. 1 passed;"));
    false
}
fn mutation(store: &TaskV2Store) -> DesktopTaskMutationParams {
    DesktopTaskMutationParams {
        actor: "fixture".into(),
        request_id: "request-once".into(),
        payload_digest: "a".repeat(64),
        expected_revision: store.desktop_task_revision("ORB-00000").unwrap(),
        fields: orbit_types::desktop::DesktopTaskFields {
            title: Some("new title".into()),
            description: Some("new description".into()),
            acceptance_criteria: Some(vec!["new criterion".into()]),
            ..Default::default()
        },
        comment: Some("structured fixture verdict".into()),
        status: Some(TaskStatus::Done),
    }
}
#[test]
fn desktop_journal_rolls_back_fields_verdict_status_and_receipt() {
    if !isolated("desktop_journal_rolls_back_fields_verdict_status_and_receipt") {
        return;
    }
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    store
        .create_task(create_params("Original", TaskStatus::Review))
        .unwrap();
    let before = store.get_task("ORB-00000").unwrap().unwrap();
    let before_revision = store.desktop_task_revision("ORB-00000").unwrap();
    let comments_before = store.get_task_comments("ORB-00000").unwrap().unwrap().len();
    let params = mutation(&store);
    inject_bundle_write_faults(&[BundleWriteFault::AfterEnvelopeStage]);
    assert!(
        store
            .apply_desktop_task_mutation("ORB-00000", &params)
            .is_err()
    );
    assert_eq!(store.get_task("ORB-00000").unwrap().unwrap(), before);
    assert_eq!(
        store.desktop_task_revision("ORB-00000").unwrap(),
        before_revision
    );
    assert_eq!(
        store.get_task_comments("ORB-00000").unwrap().unwrap().len(),
        comments_before
    );
    assert_eq!(
        store
            .apply_desktop_task_mutation("ORB-00000", &params)
            .unwrap(),
        AtomicTaskMutationOutcome::Applied
    );
    assert_eq!(
        store.get_task("ORB-00000").unwrap().unwrap().status,
        TaskStatus::Done
    );
    assert_eq!(
        store
            .apply_desktop_task_mutation("ORB-00000", &params)
            .unwrap(),
        AtomicTaskMutationOutcome::AlreadyApplied
    );
    assert_eq!(
        store.get_task_comments("ORB-00000").unwrap().unwrap().len(),
        comments_before + 1
    );
    let mut changed = params;
    changed.payload_digest = "b".repeat(64);
    assert!(
        store
            .apply_desktop_task_mutation("ORB-00000", &changed)
            .is_err()
    );
}
#[test]
fn desktop_revision_includes_comments_and_artifact_manifest() {
    if !isolated("desktop_revision_includes_comments_and_artifact_manifest") {
        return;
    }
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    store
        .create_task(create_params("Original", TaskStatus::Review))
        .unwrap();
    let mut stale = mutation(&store);
    stale.status = None;
    store
        .update_task_history(
            "ORB-00000",
            &TaskHistoryUpdateParams {
                actor: "other".into(),
                append_comments: vec![TaskComment {
                    at: Utc::now(),
                    by: "other".into(),
                    message: "concurrent".into(),
                }],
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        store
            .apply_desktop_task_mutation("ORB-00000", &stale)
            .unwrap(),
        AtomicTaskMutationOutcome::Stale
    );
    let before = store.desktop_task_revision("ORB-00000").unwrap();
    store
        .upsert_task_artifacts(
            "ORB-00000",
            &TaskArtifactUpdateParams {
                actor: "fixture".into(),
                upsert_artifacts: vec![TaskArtifact::from_text("evidence.txt", "current evidence")],
                ..Default::default()
            },
        )
        .unwrap();
    assert_ne!(store.desktop_task_revision("ORB-00000").unwrap(), before);
}
#[test]
fn desktop_read_works_with_readonly_registry_and_disables_writes() {
    if !isolated("desktop_read_works_with_readonly_registry_and_disables_writes") {
        return;
    }
    let temp = TempDir::new().unwrap();
    let writable = store(&temp);
    writable
        .create_task(create_params("Read-only fixture", TaskStatus::Review))
        .unwrap();
    let before = writable.read_desktop_task("ORB-00000").unwrap();
    let readonly = TaskV2Store::new(
        TaskRegistryStore::open_read_only(&task_registry_path(temp.path())).unwrap(),
        "orbit-test-123456".into(),
    );
    let snapshot = readonly.read_desktop_task("ORB-00000").unwrap();
    assert_eq!(snapshot.task, before.task);
    assert_eq!(snapshot.revision, before.revision);
    assert_eq!(snapshot.comments.len(), before.comments.len());
    assert!(snapshot.write_disabled_reason.is_some());
}
