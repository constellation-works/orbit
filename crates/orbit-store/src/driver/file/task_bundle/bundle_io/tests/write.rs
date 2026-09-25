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
fn write_and_read_bundle_round_trips_v2_shape() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    let bundle = sample_bundle("ORB-00000");

    let created = store.create_bundle(&bundle).expect("create bundle");
    assert_eq!(created.binding.task_id, "ORB-00000");

    let read = store.read_bundle("ORB-00000").expect("read bundle");
    assert_eq!(read.envelope, bundle.envelope);
    assert_eq!(read.description, bundle.description);
    assert_eq!(read.acceptance, bundle.acceptance);
    assert_eq!(read.plan, bundle.plan);
    assert_eq!(read.events, bundle.events);
    assert_eq!(read.comments, bundle.comments);
    assert!(
        created
            .binding
            .canonical_path
            .join(TASK_ENVELOPE_FILE_NAME)
            .is_file()
    );
    assert!(
        created
            .binding
            .canonical_path
            .join(TASK_ARTIFACTS_DIR_NAME)
            .join(TASK_ARTIFACT_FILES_DIR_NAME)
            .is_dir()
    );
}

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

#[test]
fn erofs_bundle_publish_names_path_and_hints_sandbox() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    let bundle = sample_bundle("ORB-00000");
    let bundle_dir = store.bundle_path("ORB-00000").expect("bundle path");

    let error = write_bundle_atomically(&bundle_dir, &bundle, None, |_staging, _final_path| {
        Err(std::io::Error::new(
            std::io::ErrorKind::ReadOnlyFilesystem,
            "Read-only file system",
        ))
    })
    .expect_err("EROFS publish must fail");

    match error {
        OrbitError::Io(message) => {
            assert!(
                message.contains(&bundle_dir.display().to_string()),
                "expected path in `{message}`"
            );
            assert!(
                message.contains("is not writable"),
                "expected writable attribution in `{message}`"
            );
            assert!(
                message.contains("sandbox or environment"),
                "expected sandbox/environment hint in `{message}`"
            );
            assert!(
                message.contains("not an Orbit store defect"),
                "expected store-defect negation in `{message}`"
            );
        }
        other => panic!("expected Io, got {other}"),
    }
    assert!(
        !bundle_dir.exists(),
        "an EROFS writer must not expose the canonical bundle path"
    );
}

/// `write_yaml_atomic_with` is bound to durable `atomic_write_text`, so both
/// bundle create and envelope rewrite fsync the `task.yaml` temp file before
/// rename. When `strace` is available this asserts the syscall order; a
/// sandbox denial is not a product failure.
#[test]
fn task_yaml_temp_is_fsynced_before_rename_on_create_and_rewrite() {
    exercise_task_yaml_create_and_rewrite();

    #[cfg(target_os = "linux")]
    trace_task_yaml_fsync_before_rename();
}

fn exercise_task_yaml_create_and_rewrite() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    store
        .create_bundle(&sample_bundle("ORB-00000"))
        .expect("create bundle");
    let mut envelope = sample_bundle("ORB-00000").envelope;
    envelope.title = "Rewritten".to_string();
    store
        .rewrite_envelope("ORB-00000", &envelope)
        .expect("rewrite envelope");
    let read = store.read_bundle("ORB-00000").expect("read");
    assert_eq!(read.envelope.title, "Rewritten");
}

#[cfg(target_os = "linux")]
fn trace_task_yaml_fsync_before_rename() {
    use std::process::Command;

    if std::env::var_os("ORB_TASK_YAML_FSYNC_PROBE").is_some() {
        return;
    }
    let Ok(version) = Command::new("strace").arg("-V").output() else {
        return;
    };
    if !version.status.success() {
        return;
    }
    let log = tempfile::NamedTempFile::new().expect("strace log");
    let Some(test_name) = std::thread::current().name().map(ToOwned::to_owned) else {
        return;
    };
    let output = Command::new("strace")
        .args([
            "-f",
            "-y",
            "-e",
            "trace=fsync,fdatasync,rename,renameat,renameat2",
            "-o",
        ])
        .arg(log.path())
        .arg(std::env::current_exe().expect("test exe"))
        .args(["--exact", &test_name])
        .env("ORB_TASK_YAML_FSYNC_PROBE", "1")
        .env("RUST_TEST_THREADS", "1")
        .output();
    let output = match output {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => return,
        Err(_) => return,
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Operation not permitted")
            || stderr.contains("Permission denied")
            || stderr.contains("not permitted")
        {
            return;
        }
        // Probe ran the test body; parse the log even if the harness exit is noisy.
    }
    let trace = fs::read_to_string(log.path()).unwrap_or_default();
    if trace.is_empty() {
        return;
    }
    assert_task_yaml_tmp_fsynced_before_rename(&trace);
}

#[cfg(target_os = "linux")]
fn assert_task_yaml_tmp_fsynced_before_rename(trace: &str) {
    let mut last_rename_line = 0usize;
    let mut last_fsync_line = 0usize;
    let mut renames = 0usize;
    for (index, line) in trace.lines().enumerate() {
        let number = index + 1;
        if (line.contains("fsync") || line.contains("fdatasync")) && !line.contains("unfinished") {
            last_fsync_line = number;
        }
        if is_task_yaml_tmp_rename(line) {
            assert!(
                last_fsync_line > last_rename_line,
                "task.yaml temp rename without a preceding fsync (rename line {number}): {line}\n{trace}"
            );
            last_rename_line = number;
            renames += 1;
        }
    }
    assert!(
        renames >= 2,
        "expected fsynced create and rewrite renames of task.yaml, found {renames}:\n{trace}"
    );
}

#[cfg(target_os = "linux")]
fn is_task_yaml_tmp_rename(line: &str) -> bool {
    let is_rename =
        line.contains("rename(") || line.contains("renameat(") || line.contains("renameat2(");
    is_rename && line.contains(".task.yaml.") && line.contains(".tmp") && line.contains("task.yaml")
}
