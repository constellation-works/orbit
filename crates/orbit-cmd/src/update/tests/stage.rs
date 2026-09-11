use std::io;

use crate::update::stage::restore_backup_with_rename;

#[test]
fn a_failed_atomic_restore_keeps_both_complete_files_and_cleans_staging() {
    let root = tempfile::tempdir().expect("fixture root");
    let destination = root.path().join("orbit");
    let backup = root.path().join("orbit.previous");
    let replacement = b"#!/bin/sh\nprintf 'orbit replacement-complete\\n'\n";
    let previous = b"#!/bin/sh\nprintf 'orbit previous-complete\\n'\n";
    std::fs::write(&destination, replacement).expect("write replacement");
    std::fs::write(&backup, previous).expect("write backup");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        for path in [&destination, &backup] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
                .expect("make fixture executable runnable");
        }
    }

    let error = restore_backup_with_rename(&destination, &backup, |_, _| {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "injected rename failure",
        ))
    })
    .expect_err("restore should fail");

    assert_eq!(
        std::fs::read(&destination).expect("read destination"),
        replacement
    );
    assert_eq!(std::fs::read(&backup).expect("read backup"), previous);
    #[cfg(unix)]
    {
        let installed = std::process::Command::new(&destination)
            .output()
            .expect("launch intact replacement");
        let retained = std::process::Command::new(&backup)
            .output()
            .expect("launch retained backup");
        assert!(installed.status.success());
        assert!(retained.status.success());
        assert_eq!(installed.stdout, b"orbit replacement-complete\n");
        assert_eq!(retained.stdout, b"orbit previous-complete\n");
    }
    assert!(!restore_staging_file_remains(root.path()));
    let message = error.to_string();
    assert!(message.contains("atomically replace"), "{message}");
    assert!(message.contains("was left intact"), "{message}");
    assert!(message.contains(&backup.display().to_string()), "{message}");
    assert!(message.contains("staging file was removed"), "{message}");
}

#[cfg(target_os = "linux")]
#[test]
fn rollback_replaces_a_running_executable_atomically() {
    use crate::update::stage::restore_backup;
    use orbit_common::test_process::retry_executable_busy;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::time::{Duration, Instant};

    let root = tempfile::tempdir().expect("fixture root");
    let destination = root.path().join("orbit");
    let backup = root.path().join("orbit.previous");
    let ready = root.path().join("replacement.ready");
    let release = root.path().join("replacement.release");
    std::fs::copy(
        std::env::current_exe().expect("current test executable"),
        &destination,
    )
    .expect("install replacement test executable");
    let previous = b"#!/bin/sh\nprintf 'orbit previous-complete\\n'\n";
    std::fs::write(&backup, previous).expect("write previous executable");
    std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o751))
        .expect("make previous executable runnable");

    let mut command = Command::new(&destination);
    command
        .args([
            "--ignored",
            "--exact",
            "update::tests::stage::running_replacement_process_fixture",
        ])
        .env("ORBIT_TEST_REPLACEMENT_READY", &ready)
        .env("ORBIT_TEST_REPLACEMENT_RELEASE", &release);
    let mut running =
        retry_executable_busy(|| command.spawn()).expect("launch installed replacement");
    wait_for_path(&ready, Duration::from_secs(5));

    restore_backup(&destination, &backup).expect("atomically restore previous executable");
    std::fs::write(&release, b"release").expect("release running replacement");
    assert!(running.wait().expect("wait for replacement").success());

    assert_eq!(
        std::fs::read(&destination).expect("read restored executable"),
        previous
    );
    assert_eq!(
        std::fs::read(&backup).expect("read retained backup"),
        previous
    );
    assert_eq!(
        std::fs::metadata(&destination)
            .expect("restored metadata")
            .permissions()
            .mode()
            & 0o777,
        0o751
    );
    assert!(!restore_staging_file_remains(root.path()));
    let output = Command::new(&destination)
        .output()
        .expect("launch restored executable");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"orbit previous-complete\n");

    fn wait_for_path(path: &std::path::Path, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "subprocess fixture for rollback_replaces_a_running_executable_atomically"]
fn running_replacement_process_fixture() {
    use std::time::{Duration, Instant};

    let Some(ready) = std::env::var_os("ORBIT_TEST_REPLACEMENT_READY") else {
        return;
    };
    let release =
        std::env::var_os("ORBIT_TEST_REPLACEMENT_RELEASE").expect("replacement release path");
    std::fs::write(&ready, b"ready").expect("signal replacement is running");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !std::path::Path::new(&release).exists() {
        assert!(Instant::now() < deadline, "timed out waiting for release");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn restore_staging_file_remains(directory: &std::path::Path) -> bool {
    std::fs::read_dir(directory)
        .expect("read fixture directory")
        .filter_map(Result::ok)
        .any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".orbit-update-restore")
        })
}
