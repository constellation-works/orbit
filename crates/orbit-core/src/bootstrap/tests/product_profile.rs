//! Product identity is checked before bootstrap changes any selected root.

use std::fs;
use std::path::Path;

use crate::OrbitRuntime;
use crate::bootstrap::init::{InitOptions, init_workspace_at_root};
use crate::bootstrap::product_profile::{
    PRODUCT_MARKER, ProductProfile, initialize_research_catalog,
};
use crate::runtime::OrbitRuntimeRoots;

fn in_child(test: &str, body: impl FnOnce(&Path)) {
    const CHILD: &str = "ORBIT_TEST_PRODUCT_PROFILE_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(test) {
        body(&std::env::current_dir().expect("fixture cwd"));
        return;
    }
    let temp = tempfile::tempdir().expect("fixture directory");
    let home = temp.path().join("home");
    fs::create_dir(&home).expect("fixture home");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test executable"));
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command
        .arg(test)
        .arg("--exact")
        .arg("--nocapture")
        .env(CHILD, test)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .current_dir(temp.path())
        .output()
        .expect("isolated test child");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn roots(base: &Path) -> OrbitRuntimeRoots {
    OrbitRuntimeRoots {
        global_root: base.join("global"),
        shared_root: base.join("shared"),
        local_root: base.join("local"),
    }
}

#[test]
fn research_catalog_reentry_preserves_custom_work_without_engineering_defaults() {
    in_child(
        "bootstrap::tests::product_profile::research_catalog_reentry_preserves_custom_work_without_engineering_defaults",
        |base| {
            let roots = roots(base);
            initialize_research_catalog(&roots).expect("first research bootstrap");
            let note = roots.shared_root.join("user-note.md");
            fs::write(&note, "preserve me").expect("custom record");
            initialize_research_catalog(&roots).expect("second research bootstrap");
            assert_eq!(
                fs::read_to_string(note).expect("custom record"),
                "preserve me"
            );
            for root in [&roots.global_root, &roots.shared_root, &roots.local_root] {
                assert_eq!(
                    fs::read_to_string(root.join(PRODUCT_MARKER)).expect("identity"),
                    ProductProfile::ResearchFixture.identity()
                );
                for name in [
                    "resources/jobs",
                    "resources/activities",
                    "resources/executors",
                    "skills",
                    "routines",
                    "auto_tasks",
                    "resources/.orbit-global-defaults.json",
                    "config.toml",
                ] {
                    assert!(
                        !root.join(name).exists(),
                        "unexpected engineering asset: {}",
                        root.join(name).display()
                    );
                }
            }
            assert!(
                roots
                    .global_root
                    .join("resources/policies/default.yaml")
                    .exists()
            );
            assert!(OrbitRuntime::initialize_from_resolved_roots(roots, None).is_err());
        },
    );
}

#[test]
fn product_mismatch_refuses_all_roots_before_generation_or_forced_init() {
    in_child(
        "bootstrap::tests::product_profile::product_mismatch_refuses_all_roots_before_generation_or_forced_init",
        |base| {
            let roots = roots(base);
            ProductProfile::ResearchFixture
                .claim_root(&roots.shared_root)
                .expect("research root");
            let marker = fs::read(roots.shared_root.join(PRODUCT_MARKER)).expect("identity");
            assert!(OrbitRuntime::initialize_from_resolved_roots(roots.clone(), None).is_err());
            assert!(
                !roots.global_root.exists(),
                "generation/bootstrap changed other root"
            );
            assert!(!roots.local_root.exists());
            assert!(
                init_workspace_at_root(
                    &roots.shared_root,
                    InitOptions {
                        force: true,
                        global_only: true,
                        ..Default::default()
                    }
                )
                .is_err()
            );
            assert_eq!(
                fs::read(roots.shared_root.join(PRODUCT_MARKER)).expect("identity remains"),
                marker
            );
            assert!(
                crate::application::workspace_sync::reconcile_workspace_managed_artifacts(
                    &roots.global_root,
                    &roots.shared_root,
                    None,
                    "agent-main",
                    false
                )
                .is_err()
            );
            assert!(
                !roots.global_root.exists(),
                "reconciliation changed other root"
            );
            let untouched = base.join("existing-orbit-workspace");
            fs::create_dir(&untouched).expect("existing workspace");
            fs::write(untouched.join("keep"), "do not delete").expect("existing work");
            assert!(
                init_workspace_at_root(
                    &untouched,
                    InitOptions {
                        force: true,
                        global_root_override: Some(roots.shared_root.clone()),
                        ..Default::default()
                    }
                )
                .is_err()
            );
            assert_eq!(
                fs::read_to_string(untouched.join("keep")).expect("work remains"),
                "do not delete"
            );
            assert!(!untouched.join(PRODUCT_MARKER).exists());
        },
    );
}

#[test]
fn research_refuses_unmarked_legacy_roots_before_claiming_other_roots() {
    in_child(
        "bootstrap::tests::product_profile::research_refuses_unmarked_legacy_roots_before_claiming_other_roots",
        |base| {
            let roots = roots(base);
            fs::create_dir(&roots.local_root).expect("legacy root");
            fs::write(roots.local_root.join("config.toml"), "legacy").expect("legacy config");
            assert!(initialize_research_catalog(&roots).is_err());
            assert!(!roots.global_root.exists());
            assert!(!roots.shared_root.exists());
            assert!(!roots.local_root.join(PRODUCT_MARKER).exists());
            ProductProfile::Orbit
                .validate_roots(&[&roots.local_root])
                .expect("legacy Orbit root remains supported");
        },
    );
}

#[test]
fn foreign_ancestors_and_symlink_aliases_are_not_independent_roots() {
    in_child(
        "bootstrap::tests::product_profile::foreign_ancestors_and_symlink_aliases_are_not_independent_roots",
        |base| {
            let owner = base.join("orbit-owner");
            ProductProfile::Orbit
                .claim_root(&owner)
                .expect("Orbit owner");
            let nested = roots(&owner.join("nested"));
            assert!(initialize_research_catalog(&nested).is_err());
            assert!(!owner.join("nested").exists());
            ProductProfile::Orbit
                .validate_roots(&[&owner.join("child")])
                .expect("same product child");
            #[cfg(unix)]
            {
                let subdir = owner.join("subdir");
                fs::create_dir(&subdir).expect("owned subdir");
                let alias = base.join("alias");
                std::os::unix::fs::symlink(&subdir, &alias).expect("root alias");
                assert!(initialize_research_catalog(&roots(&alias)).is_err());
                assert!(!subdir.join("global").exists());
            }
        },
    );
}

#[test]
fn malformed_and_symlink_markers_fail_closed() {
    in_child(
        "bootstrap::tests::product_profile::malformed_and_symlink_markers_fail_closed",
        |base| {
            let root = base.join("bad");
            fs::create_dir(&root).expect("root");
            fs::write(root.join(PRODUCT_MARKER), "").expect("interrupted marker write");
            assert!(ProductProfile::Orbit.validate_roots(&[&root]).is_err());
            #[cfg(unix)]
            {
                fs::remove_file(root.join(PRODUCT_MARKER)).expect("remove bad marker");
                let target = base.join("marker-target");
                fs::write(&target, ProductProfile::Orbit.identity()).expect("valid foreign marker");
                std::os::unix::fs::symlink(&target, root.join(PRODUCT_MARKER))
                    .expect("marker alias");
                assert!(ProductProfile::Orbit.validate_roots(&[&root]).is_err());
            }
        },
    );
}

#[test]
fn research_explicit_single_root_reopens_without_guessing_roots() {
    in_child(
        "bootstrap::tests::product_profile::research_explicit_single_root_reopens_without_guessing_roots",
        |base| {
            let single = base.join("single");
            let roots = OrbitRuntimeRoots {
                global_root: single.clone(),
                shared_root: single.clone(),
                local_root: single.clone(),
            };
            initialize_research_catalog(&roots).expect("single explicit root");
            initialize_research_catalog(&roots).expect("same root reopens");
            assert_eq!(
                fs::read_to_string(single.join(PRODUCT_MARKER)).expect("identity"),
                ProductProfile::ResearchFixture.identity()
            );
            assert!(!base.join("home/.orbit").exists());
            assert!(
                ProductProfile::ResearchFixture
                    .validate_roots(&[Path::new("relative")])
                    .is_err()
            );
            assert!(
                ProductProfile::ResearchFixture
                    .validate_roots(&[&base.join("child/../escape")])
                    .is_err()
            );
        },
    );
}
