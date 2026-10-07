use std::borrow::Cow;
use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

use super::super::managed_assets::MANAGED_ASSET_MANIFEST_FILE;
use crate::OrbitRuntime;
use crate::application::routines::seed::RoutineSeedIdentity;
use crate::bootstrap::init::{InitOptions, InitResult, init_workspace_at_root};
use orbit_config::ConfigSeed;

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

fn init_global(root: &Path) -> InitResult {
    init_workspace_at_root(
        root,
        InitOptions {
            global_only: true,
            refresh_defaults: true,
            config_seed: Some(ConfigSeed::default()),
            ..Default::default()
        },
    )
    .expect("initialize global defaults")
}

fn sha256(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

fn add_managed_manifest_entry(dir: &Path, name: &str, content: &str) {
    let manifest_path = dir.join(MANAGED_ASSET_MANIFEST_FILE);
    let raw = std::fs::read_to_string(&manifest_path).expect("read managed manifest");
    let mut manifest: Value = serde_json::from_str(&raw).expect("parse managed manifest");
    manifest["assets"]
        .as_object_mut()
        .expect("manifest assets object")
        .insert(name.to_string(), Value::String(sha256(content)));
    std::fs::write(
        manifest_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest).expect("serialize managed manifest")
        ),
    )
    .expect("write previous managed manifest");
}

// --- Definition-artifact health [ORB-10800] ---------------------------------

mod artifacts {
    use super::*;
    use std::path::PathBuf;

    fn init_workspace(root: &Path) -> (PathBuf, PathBuf) {
        let global_root = root.join("global");
        let workspace_root = root.join("repo/.orbit");
        init_global(&global_root);
        init_workspace_at_root(
            &workspace_root,
            InitOptions {
                refresh_defaults: true,
                global_root_override: Some(global_root.clone()),
                routine_seed_identity: Some(
                    RoutineSeedIdentity::new("repo", "hm_test", "main")
                        .expect("routine seed identity"),
                ),
                config_seed: Some(ConfigSeed::default()),
                ..Default::default()
            },
        )
        .expect("initialize workspace");
        (global_root, workspace_root)
    }

    /// Removal refuses a manifest key that would escape the managed directory,
    /// and never follows a symlink at the boundary.
    #[test]
    fn removal_rejects_escaping_paths_and_does_not_follow_symlinks() {
        if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
            &removal_rejects_escaping_paths_and_does_not_follow_symlinks,
        )) {
            return;
        }
        let root = tempdir().expect("create tempdir");
        let (global_root, workspace_root) = init_workspace(root.path());
        let auto_tasks_dir = workspace_root.join("auto_tasks");

        // A traversing key is refused when the manifest is loaded, before any
        // removal is attempted.
        let manifest_path = auto_tasks_dir.join(MANAGED_ASSET_MANIFEST_FILE);
        let mut manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).expect("read manifest"))
                .expect("parse manifest");
        manifest["assets"]
            .as_object_mut()
            .expect("assets object")
            .insert("../escaped".to_string(), Value::String(sha256("anything")));
        std::fs::write(
            &manifest_path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&manifest).expect("serialize manifest")
            ),
        )
        .expect("write traversing manifest");

        let runtime =
            OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
        let error = runtime
            .remove_stale_definition_artifacts()
            .expect_err("a traversing manifest key must be refused");
        assert!(
            error.to_string().contains("not a safe managed asset path"),
            "{error}"
        );

        // A symlinked artifact is left in place: deleting it would act on a
        // target outside the catalog Orbit manages.
        #[cfg(unix)]
        {
            std::fs::write(
                &manifest_path,
                format!(
                    "{}\n",
                    serde_json::to_string_pretty(&{
                        let mut clean: Value = serde_json::from_str(
                            &std::fs::read_to_string(&manifest_path).expect("read manifest"),
                        )
                        .expect("parse manifest");
                        clean["assets"]
                            .as_object_mut()
                            .expect("assets object")
                            .remove("../escaped");
                        clean
                    })
                    .expect("serialize manifest")
                ),
            )
            .expect("restore manifest");

            let outside = root.path().join("outside.yaml");
            std::fs::write(&outside, "outside content\n").expect("write link target");
            let link = auto_tasks_dir.join("linked-auto-task.yaml");
            std::os::unix::fs::symlink(&outside, &link).expect("create symlinked artifact");
            add_managed_manifest_entry(&auto_tasks_dir, "linked-auto-task", "outside content\n");

            let runtime =
                OrbitRuntime::from_roots(&global_root, &workspace_root).expect("rebuild runtime");
            assert_eq!(
                runtime
                    .remove_stale_definition_artifacts()
                    .expect("retire pass"),
                0,
                "a symlinked artifact is not removed"
            );
            assert!(link.symlink_metadata().is_ok(), "the symlink survives");
            assert_eq!(
                std::fs::read_to_string(&outside).expect("link target survives"),
                "outside content\n"
            );
        }
    }
}
