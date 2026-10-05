//! Admitted fault injection: force a writer between archive preflight and the
//! packing recheck, which a public export fixture cannot schedule economically.

use std::cell::RefCell;
use std::fs;
use std::process::Command;
use std::time::Duration;

use orbit_common::{process, test_env};
use tempfile::TempDir;

use super::super::archive::write_archive;
use crate::driver::file::task_bundle::PENDING_WRITE_FILE_NAME;

thread_local! {
    static AFTER_MANIFEST: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

pub(in super::super) fn after_manifest() {
    let hook = AFTER_MANIFEST.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

#[test]
fn packing_failure_preserves_destination_and_cleans_staging_file() {
    const TEST: &str = "workflow::task::tests::archive::packing_failure_preserves_destination_and_cleans_staging_file";
    const MARKER: &str = "ORBIT_TEST_ARCHIVE_FAILURE_CHILD";
    if std::env::var(MARKER).as_deref() != Ok(TEST) {
        let home = TempDir::new().unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        test_env::clear_inherited_authority(|key| {
            command.env_remove(key);
        });
        command
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(MARKER, TEST)
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .current_dir(home.path());
        let output =
            process::run_bounded_capped(&mut command, Duration::from_secs(120), 256 * 1024)
                .expect("run isolated archive failure fixture");
        test_env::assert_child_test_passed(TEST, output.status, output.stdout, output.stderr);
        return;
    }

    for existing_backup in [false, true] {
        for disappear in [false, true] {
            let root = TempDir::new().unwrap();
            let bundle = root.path().join("bundle");
            fs::create_dir(&bundle).unwrap();
            fs::write(bundle.join("task.md"), b"task payload").unwrap();
            let output_dir = root.path().join("output");
            fs::create_dir(&output_dir).unwrap();
            let archive = output_dir.join("tasks.tar.zst");
            let previous = b"previous complete backup\0\xff";
            if existing_backup {
                fs::write(&archive, previous).unwrap();
            }
            let changed_bundle = bundle.clone();
            let moved_bundle = root.path().join("moved-bundle");
            AFTER_MANIFEST.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || {
                    if disappear {
                        fs::rename(&changed_bundle, &moved_bundle).unwrap();
                    } else {
                        fs::write(changed_bundle.join(PENDING_WRITE_FILE_NAME), b"pending")
                            .unwrap();
                    }
                }));
            });

            let error = write_archive(&archive, b"{}", &[("ORB-00000".into(), bundle)])
                .expect_err("a bundle changed after packing begins must fail export");
            let expected = if disappear {
                "disappeared"
            } else {
                "pending-write"
            };
            assert!(error.to_string().contains(expected), "{error}");
            if existing_backup {
                assert_eq!(
                    fs::read(&archive).unwrap(),
                    previous,
                    "packing errors must preserve the previous backup byte for byte"
                );
            } else {
                assert!(
                    !archive.exists(),
                    "packing errors must not publish a partial archive"
                );
            }
            assert_eq!(
                fs::read_dir(&output_dir).unwrap().count(),
                usize::from(existing_backup),
                "packing errors must remove the sibling staging file"
            );
        }
    }
}
