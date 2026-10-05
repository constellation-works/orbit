use std::fs;

use orbit_common::OrbitError;
use orbit_types::task::{
    TASK_ARTIFACT_FILES_DIR_NAME, TASK_ARTIFACTS_DIR_NAME, TASK_ENVELOPE_FILE_NAME,
    TASK_EVENTS_FILE_NAME,
};
use tempfile::TempDir;

use super::super::write::write_bundle_atomically;
use crate::repository::task::tests::test_support::{bundle_store, sample_bundle};

#[test]
fn interrupted_bundle_publish_never_exposes_partial_final_directory() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    let bundle = sample_bundle("ORB-00000");
    let bundle_dir = store.bundle_path("ORB-00000").expect("bundle path");

    let error = write_bundle_atomically(&bundle_dir, &bundle, None, |staging, final_path| {
        assert!(
            !final_path.exists(),
            "final path must remain absent while staging"
        );
        assert!(staging.join(TASK_ENVELOPE_FILE_NAME).is_file());
        assert!(staging.join(TASK_EVENTS_FILE_NAME).is_file());
        assert!(
            staging
                .join(TASK_ARTIFACTS_DIR_NAME)
                .join(TASK_ARTIFACT_FILES_DIR_NAME)
                .is_dir()
        );
        Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "simulated interruption before rename",
        ))
    })
    .expect_err("interrupted publish must fail");

    assert!(matches!(error, OrbitError::Io(message) if message.contains("simulated interruption")));
    assert!(
        !bundle_dir.exists(),
        "an interrupted writer must not expose the canonical bundle path"
    );
    let parent = bundle_dir.parent().expect("bundle parent");
    let staging_entries = fs::read_dir(parent)
        .expect("read bundle parent")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".staging"))
        .collect::<Vec<_>>();
    assert!(
        staging_entries.is_empty(),
        "handled failures clean their private staging directories"
    );
}
