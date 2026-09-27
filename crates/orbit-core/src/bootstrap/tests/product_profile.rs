//! Product identity is checked before bootstrap changes any selected root.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

use crate::OrbitRuntime;
use crate::bootstrap::init::{InitOptions, init_workspace_at_root};
use crate::bootstrap::product_profile::{
    PRODUCT_MARKER, ProductProfile, arm_marker_planted_before_link, arm_marker_publish_failure,
    clear_marker_publish_faults, initialize_research_catalog,
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

#[test]
fn concurrent_same_product_claims_publish_only_a_complete_marker() {
    in_child(
        "bootstrap::tests::product_profile::concurrent_same_product_claims_publish_only_a_complete_marker",
        |base| {
            const CLAIMERS: usize = 8;
            const ROUNDS: usize = 16;
            let identity = ProductProfile::Orbit.identity().as_bytes().to_vec();
            for round in 0..ROUNDS {
                let root = base.join(format!("round-{round}"));
                fs::create_dir(&root).expect("round root");
                let marker = root.join(PRODUCT_MARKER);
                let stop = Arc::new(AtomicBool::new(false));
                let partial = Arc::new(Mutex::new(None));
                let reader = thread::spawn({
                    let marker = marker.clone();
                    let stop = Arc::clone(&stop);
                    let partial = Arc::clone(&partial);
                    let identity = identity.clone();
                    move || {
                        while !stop.load(Ordering::Acquire) {
                            match fs::read(&marker) {
                                Ok(bytes) if bytes == identity => {}
                                Ok(bytes) => {
                                    *partial.lock().expect("partial observation") = Some(bytes);
                                    return;
                                }
                                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                                Err(error) => {
                                    *partial.lock().expect("partial observation") =
                                        Some(format!("read error: {error}").into_bytes());
                                    return;
                                }
                            }
                        }
                    }
                });
                let start = Arc::new(Barrier::new(CLAIMERS));
                let mut claimers = Vec::with_capacity(CLAIMERS);
                for _ in 0..CLAIMERS {
                    let root = root.clone();
                    let start = Arc::clone(&start);
                    claimers.push(thread::spawn(move || {
                        start.wait();
                        ProductProfile::Orbit.claim_root(&root)
                    }));
                }
                for claimer in claimers {
                    claimer
                        .join()
                        .expect("claimer thread")
                        .expect("same-product claim");
                }
                stop.store(true, Ordering::Release);
                reader.join().expect("reader thread");
                let observed = partial.lock().expect("partial observation").clone();
                assert!(
                    observed.is_none(),
                    "round {round} observed {}",
                    String::from_utf8_lossy(observed.as_deref().unwrap_or_default())
                );
                assert_eq!(fs::read(&marker).expect("published marker"), identity);
                assert!(
                    non_marker_entries(&root).is_empty(),
                    "round {round} left {:?}",
                    non_marker_entries(&root)
                );
                #[cfg(unix)]
                let published_inode = {
                    use std::os::unix::fs::MetadataExt;
                    fs::symlink_metadata(&marker)
                        .expect("marker metadata")
                        .ino()
                };
                ProductProfile::Orbit
                    .claim_root(&root)
                    .expect("reentry keeps the published marker");
                assert_eq!(fs::read(&marker).expect("marker after reentry"), identity);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    assert_eq!(
                        fs::symlink_metadata(&marker)
                            .expect("marker metadata")
                            .ino(),
                        published_inode,
                        "reentry replaced the marker inode"
                    );
                }
            }
        },
    );
}

#[test]
fn failed_publication_leaves_no_marker_and_a_retry_succeeds() {
    in_child(
        "bootstrap::tests::product_profile::failed_publication_leaves_no_marker_and_a_retry_succeeds",
        |base| {
            let _clear = ClearPublishFaults;
            let root = base.join("root");
            arm_marker_publish_failure();
            assert!(
                ProductProfile::Orbit.claim_root(&root).is_err(),
                "injected publication failure must refuse the claim"
            );
            assert!(
                !root.join(PRODUCT_MARKER).exists(),
                "failed publication left a marker"
            );
            assert!(
                non_marker_entries(&root).is_empty(),
                "failed publication left {:?}",
                non_marker_entries(&root)
            );
            clear_marker_publish_faults();
            ProductProfile::Orbit
                .claim_root(&root)
                .expect("retry after a failed publication");
            assert_eq!(
                fs::read_to_string(root.join(PRODUCT_MARKER)).expect("identity"),
                ProductProfile::Orbit.identity()
            );
            assert!(non_marker_entries(&root).is_empty());
        },
    );
}

#[test]
fn marker_planted_before_link_is_not_replaced() {
    in_child(
        "bootstrap::tests::product_profile::marker_planted_before_link_is_not_replaced",
        |base| {
            let _clear = ClearPublishFaults;
            let root = base.join("root");
            let planted = b"orbit:".to_vec();
            arm_marker_planted_before_link(planted.clone());
            assert!(ProductProfile::Orbit.claim_root(&root).is_err());
            let marker = root.join(PRODUCT_MARKER);
            assert_eq!(fs::read(&marker).expect("planted marker"), planted);
            assert!(
                ProductProfile::Orbit.claim_root(&root).is_err(),
                "a later claim repaired the planted marker"
            );
            assert_eq!(fs::read(&marker).expect("planted marker"), planted);
            assert!(non_marker_entries(&root).is_empty());
            fs::remove_file(&marker).expect("remove planted marker");
            ProductProfile::Orbit
                .claim_root(&root)
                .expect("claim after the planted marker is gone");
            assert_eq!(
                fs::read_to_string(&marker).expect("identity"),
                ProductProfile::Orbit.identity()
            );
        },
    );
}

#[test]
fn claim_leaves_foreign_invalid_and_symlink_markers_unchanged() {
    in_child(
        "bootstrap::tests::product_profile::claim_leaves_foreign_invalid_and_symlink_markers_unchanged",
        |base| {
            refuse_regular_marker(base, "empty", b"", ProductProfile::Orbit);
            refuse_regular_marker(base, "partial", b"orbit:", ProductProfile::Orbit);
            refuse_regular_marker(
                base,
                "foreign",
                ProductProfile::ResearchFixture.identity().as_bytes(),
                ProductProfile::Orbit,
            );
            refuse_regular_marker(
                base,
                "owned-by-orbit",
                ProductProfile::Orbit.identity().as_bytes(),
                ProductProfile::ResearchFixture,
            );

            let directory = base.join("directory");
            fs::create_dir(&directory).expect("directory root");
            let directory_marker = directory.join(PRODUCT_MARKER);
            fs::create_dir(&directory_marker).expect("directory marker");
            assert!(ProductProfile::Orbit.claim_root(&directory).is_err());
            assert!(directory_marker.is_dir());
            assert!(
                fs::read_dir(&directory_marker)
                    .expect("directory marker")
                    .next()
                    .is_none()
            );
            assert!(non_marker_entries(&directory).is_empty());

            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;

                let root = base.join("symlink");
                fs::create_dir(&root).expect("symlink root");
                let target = base.join("symlink-target");
                fs::write(&target, ProductProfile::Orbit.identity()).expect("symlink target");
                let marker = root.join(PRODUCT_MARKER);
                std::os::unix::fs::symlink(&target, &marker).expect("symlink marker");
                let before = fs::symlink_metadata(&marker).expect("symlink metadata");
                assert!(ProductProfile::Orbit.claim_root(&root).is_err());
                let after = fs::symlink_metadata(&marker).expect("symlink metadata");
                assert!(after.file_type().is_symlink());
                assert_eq!(before.ino(), after.ino(), "symlink marker was replaced");
                assert_eq!(
                    fs::read_to_string(&target).expect("symlink target"),
                    ProductProfile::Orbit.identity()
                );
                assert!(non_marker_entries(&root).is_empty());
            }
        },
    );
}

fn refuse_regular_marker(base: &Path, name: &str, contents: &[u8], profile: ProductProfile) {
    let root = base.join(name);
    fs::create_dir(&root).expect(name);
    let marker = root.join(PRODUCT_MARKER);
    fs::write(&marker, contents).expect(name);
    let before = fs::symlink_metadata(&marker).expect(name);
    assert!(profile.claim_root(&root).is_err(), "{name}");
    let after = fs::symlink_metadata(&marker).expect(name);
    assert!(after.file_type().is_file(), "{name}");
    assert_eq!(fs::read(&marker).expect(name), contents, "{name}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(before.ino(), after.ino(), "{name} inode was replaced");
    }
    #[cfg(not(unix))]
    let _ = before;
    assert!(
        non_marker_entries(&root).is_empty(),
        "{name} left {:?}",
        non_marker_entries(&root)
    );
}

fn non_marker_entries(root: &Path) -> Vec<String> {
    let mut names = fs::read_dir(root)
        .expect("root entries")
        .map(|entry| {
            entry
                .expect("root entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name != PRODUCT_MARKER)
        .collect::<Vec<_>>();
    names.sort();
    names
}

struct ClearPublishFaults;

impl Drop for ClearPublishFaults {
    fn drop(&mut self) {
        clear_marker_publish_faults();
    }
}
