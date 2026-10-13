use std::borrow::Cow;

/// A failed first seed must remove its partial file so a later reconciliation
/// can create the complete managed asset.
#[cfg(unix)]
#[test]
fn failed_create_only_write_is_retried_on_the_next_reconcile() {
    const CHILD: &str = "application::tests::managed_assets::create_only_write_failure_child";
    let mut command = std::process::Command::new(std::env::current_exe().expect("test executable"));
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command.args([
        "--exact",
        CHILD,
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ]);
    let output =
        orbit_common::process::run_bounded(&mut command, std::time::Duration::from_secs(10))
            .expect("fault-injection child finishes before its deadline");
    orbit_common::test_env::assert_child_test_passed(
        CHILD,
        output.status,
        output.stdout,
        output.stderr,
    );
}

#[cfg(unix)]
#[test]
#[ignore = "runs only in the isolated file-size fault-injection child"]
fn create_only_write_failure_child() {
    use crate::application::managed_assets::{
        ManagedAssetLayout, ManagedAssetOutcome, reconcile_managed_assets,
    };

    struct FileSizeLimit(libc::rlimit);

    impl Drop for FileSizeLimit {
        fn drop(&mut self) {
            // SAFETY: restore the soft limit this isolated child lowered,
            // including if an assertion fails before the test profile is written.
            unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &self.0) };
        }
    }

    let root = tempfile::tempdir().expect("create tempdir");
    let path = root.path().join("activity.yaml");
    let content = format!("{}\n", "managed asset\n".repeat(4096));
    // SAFETY: getrlimit initializes this valid struct for the current process.
    let original = unsafe {
        let mut original: libc::rlimit = std::mem::zeroed();
        assert_eq!(libc::getrlimit(libc::RLIMIT_FSIZE, &mut original), 0);
        original
    };
    let restore_limit = FileSizeLimit(original);
    let limit = libc::rlimit {
        rlim_cur: 1024,
        rlim_max: original.rlim_max,
    };
    // SAFETY: this isolated child owns its signal disposition and resource
    // limits. `limit` is valid and lives through the synchronous write.
    unsafe {
        assert_ne!(libc::signal(libc::SIGXFSZ, libc::SIG_IGN), libc::SIG_ERR);
        assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &limit), 0);
    }

    let files = [("activity", content.as_str())];
    let error = reconcile_managed_assets(
        root.path(),
        "activity",
        ManagedAssetLayout::YamlStem,
        &files,
        false,
        |_, embedded| Ok(Cow::Borrowed(embedded)),
    )
    .expect_err("the kernel must refuse a managed asset larger than the file-size limit");
    assert!(matches!(error, orbit_common::OrbitError::Io(_)));
    assert!(
        !path.exists(),
        "a failed create-only write must remove its partial destination"
    );

    // Restore the process-wide limit before the retry and test-profile write.
    drop(restore_limit);
    let reconciled = reconcile_managed_assets(
        root.path(),
        "activity",
        ManagedAssetLayout::YamlStem,
        &files,
        false,
        |_, embedded| Ok(Cow::Borrowed(embedded)),
    )
    .expect("the next reconcile seeds the absent managed asset");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read seeded asset"),
        content
    );
    assert_eq!(reconciled.actions[0].outcome, ManagedAssetOutcome::Created);
}
