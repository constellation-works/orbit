#[cfg(target_os = "linux")]
#[test]
fn linux_git_denies_cover_linked_pointer_real_metadata_and_shared_recovery() {
    use std::fs;

    use super::super::git_sandbox::append_linux_git_denies;
    use orbit_exec::linux_bwrap_write_grant_diagnostic;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let worktree = root.join("worktree");
    let common = root.join("primary/.git");
    let git_dir = common.join("worktrees/worker");
    fs::create_dir_all(&git_dir).unwrap();
    fs::create_dir_all(&worktree).unwrap();
    fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", git_dir.display()),
    )
    .unwrap();
    fs::write(git_dir.join("commondir"), "../..\n").unwrap();
    let mut profile = orbit_types::policy::ResolvedFsProfile {
        name: "test".to_string(),
        read: vec!["/**".to_string()],
        modify: vec![format!("{}/**", root.display())],
    };
    append_linux_git_denies(&worktree, &mut profile).unwrap();
    for deny in [
        format!("!{}", worktree.join(".git").display()),
        format!("!{}/**", git_dir.display()),
        format!("!{}/**", common.display()),
    ] {
        assert!(profile.modify.contains(&deny), "{profile:?}");
    }
    assert!(
        linux_bwrap_write_grant_diagnostic(
            &profile,
            &common.join("orbit/worktree-recovery/run/manifest.json")
        )
        .unwrap()
        .is_some()
    );

    let redirected = root.join("redirected");
    fs::create_dir(&redirected).unwrap();
    std::os::unix::fs::symlink(worktree.join(".git"), redirected.join(".git")).unwrap();
    let error = append_linux_git_denies(&redirected, &mut profile).unwrap_err();
    assert!(error.to_string().contains("symlink metadata"), "{error}");
}

#[test]
fn metadata_aliases_and_invalid_pointers_fail_closed() {
    use std::fs;

    use super::super::git_sandbox::append_linux_git_denies;
    use orbit_types::policy::ResolvedFsProfile;

    for case in [
        "symlink-entry",
        "hardlink-entry",
        "hardlink-pointer",
        "invalid-pointer",
        "special-entry",
        "special-pointer",
        "invalid-common-pointer",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let git_dir = workspace.join(".git");
        fs::create_dir_all(&workspace).unwrap();
        let outside = temp.path().join("outside");
        fs::write(&outside, "host state").unwrap();
        match case {
            "symlink-entry" => {
                fs::create_dir(&git_dir).unwrap();
                std::os::unix::fs::symlink(&outside, git_dir.join("HEAD")).unwrap();
            }
            "hardlink-entry" => {
                fs::create_dir(&git_dir).unwrap();
                fs::hard_link(&outside, git_dir.join("HEAD")).unwrap();
            }
            "hardlink-pointer" => fs::hard_link(&outside, &git_dir).unwrap(),
            "invalid-pointer" => fs::write(&git_dir, "not a Git pointer").unwrap(),
            "special-entry" => {
                fs::create_dir(&git_dir).unwrap();
                std::os::unix::net::UnixListener::bind(git_dir.join("socket")).unwrap();
            }
            "special-pointer" => {
                std::os::unix::net::UnixListener::bind(&git_dir).unwrap();
            }
            "invalid-common-pointer" => {
                fs::create_dir(&git_dir).unwrap();
                fs::write(git_dir.join("commondir"), "\n").unwrap();
            }
            _ => unreachable!(),
        }
        let mut profile = ResolvedFsProfile {
            name: "test".to_string(),
            read: Vec::new(),
            modify: Vec::new(),
        };
        assert!(
            append_linux_git_denies(&workspace, &mut profile).is_err(),
            "{case}"
        );
        assert_eq!(fs::read_to_string(outside).unwrap(), "host state");
    }
}

/// Deterministic fault injection: a disappearing leaf is retryable, but no
/// other I/O failure or endless stream of disappearing locks may admit a scan.
#[test]
fn git_scan_denies_non_not_found_errors_and_persistent_disappearance() {
    use std::cell::Cell;
    use std::io;
    use std::rc::Rc;

    use super::super::git_sandbox::{GitScanHookGuard, GitScanStage, append_linux_git_denies};

    for failure in [
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::NotADirectory,
        io::ErrorKind::NotFound,
    ] {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        let objects = workspace.join(".git/objects");
        std::fs::create_dir_all(&objects).unwrap();
        let lock = objects.join("maintenance.lock");
        std::fs::write(&lock, "lock").unwrap();
        let observations = Rc::new(Cell::new(0));
        let observed = Rc::clone(&observations);
        let _hook = GitScanHookGuard::install(move |stage, path| {
            if stage == GitScanStage::ReadDirectory && path == objects {
                std::fs::write(&lock, "lock")?;
            }
            if stage == GitScanStage::InspectEntry && path == lock {
                observed.set(observed.get() + 1);
                if failure == io::ErrorKind::NotFound {
                    std::fs::remove_file(path)?;
                } else {
                    return Err(io::Error::from(failure));
                }
            }
            Ok(())
        });
        let mut profile = orbit_types::policy::ResolvedFsProfile {
            name: "test".to_string(),
            read: Vec::new(),
            modify: Vec::new(),
        };
        let error = append_linux_git_denies(&workspace, &mut profile).unwrap_err();
        assert!(matches!(error, orbit_common::OrbitError::PolicyDenied(_)));
        if failure == io::ErrorKind::NotFound {
            assert!(
                (2..=3).contains(&observations.get()),
                "persistent traversal instability must stop after bounded revalidation"
            );
        } else {
            assert_eq!(
                observations.get(),
                1,
                "non-NotFound errors must deny the first attempt without retry"
            );
        }
    }
}

/// Force replacements after an entry has vanished or a directory has been
/// inspected. A retry must validate replacement leaves and retain directory
/// identities, including the root's ancestors.
#[test]
fn git_scan_revalidation_denies_unsafe_replacements() {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::super::git_sandbox::{GitScanHookGuard, GitScanStage, append_linux_git_denies};

    for case in [
        "symlink-leaf",
        "hardlink-leaf",
        "special-leaf",
        "symlink-directory",
        "replaced-directory",
        "replaced-directory-on-retry",
        "replaced-root",
        "replaced-ancestor",
        "missing-root",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let workspace = root.join("workspace");
        let git_dir = workspace.join(".git");
        let objects = git_dir.join("objects");
        std::fs::create_dir_all(&objects).unwrap();
        let lock = objects.join("maintenance.lock");
        std::fs::write(&lock, "lock").unwrap();
        let outside = root.join("outside");
        std::fs::create_dir(&outside).unwrap();
        let host_file = outside.join("host-file");
        std::fs::write(&host_file, "host state").unwrap();
        let vanished = Rc::new(Cell::new(false));
        let replaced = Rc::new(Cell::new(false));
        let observed = Rc::clone(&replaced);
        let scan_workspace = workspace.clone();
        let scan_host_file = host_file.clone();
        let _hook = GitScanHookGuard::install(move |stage, path| {
            if case.ends_with("leaf") || case == "replaced-directory-on-retry" {
                if stage == GitScanStage::InspectEntry && path == lock && !vanished.get() {
                    std::fs::remove_file(path)?;
                    vanished.set(true);
                } else if stage == GitScanStage::ReadDirectory && path == objects && vanished.get()
                {
                    match case {
                        "symlink-leaf" => std::os::unix::fs::symlink(&scan_host_file, &lock)?,
                        "hardlink-leaf" => std::fs::hard_link(&scan_host_file, &lock)?,
                        "special-leaf" => {
                            std::os::unix::net::UnixListener::bind(&lock)?;
                        }
                        "replaced-directory-on-retry" => {
                            std::fs::rename(&objects, root.join("original"))?;
                            std::fs::create_dir(&objects)?;
                        }
                        _ => unreachable!(),
                    }
                    observed.set(true);
                }
            } else if stage == GitScanStage::ReadDirectory && path == objects && !observed.get() {
                // The parent has already inspected and pinned this directory.
                let target = match case {
                    "replaced-root" | "missing-root" => &git_dir,
                    "replaced-ancestor" => &scan_workspace,
                    _ => &objects,
                };
                std::fs::rename(target, root.join("original"))?;
                match case {
                    "symlink-directory" => std::os::unix::fs::symlink(&outside, target)?,
                    "missing-root" => {}
                    _ => {
                        std::fs::create_dir_all(&objects)?;
                    }
                }
                observed.set(true);
            }
            Ok(())
        });
        let mut profile = orbit_types::policy::ResolvedFsProfile {
            name: "test".to_string(),
            read: Vec::new(),
            modify: Vec::new(),
        };
        let error = append_linux_git_denies(&workspace, &mut profile).unwrap_err();
        assert!(
            matches!(error, orbit_common::OrbitError::PolicyDenied(_)),
            "{case}: {error}"
        );
        assert!(replaced.get(), "{case} must exercise the replacement seam");
        assert_eq!(std::fs::read_to_string(host_file).unwrap(), "host state");
    }
}

/// The task-pilot boundary [ORB-11756]. A source-inspection slot holds a
/// checkout of ordinary tracked content, symlinks included. It lives in Orbit
/// state beside the repository rather than under authoritative Git metadata,
/// so protection admits the launch while every metadata leaf stays denied.
/// Planting the same checkout back inside the metadata tree is still refused.
#[cfg(target_os = "linux")]
#[test]
fn source_inspection_slot_launches_while_git_metadata_leaves_stay_denied() {
    use std::fs;
    use std::path::Path;

    use super::super::git_sandbox::append_linux_git_denies;
    use orbit_common::fs::git::run_git;
    use orbit_exec::linux_bwrap_write_grant_diagnostic;
    use orbit_types::policy::ResolvedFsProfile;

    fn git(root: &Path, args: &[&str]) -> String {
        let output = run_git(root, args).unwrap();
        assert!(output.success, "git {args:?}: {}", output.stderr);
        output.stdout.trim().to_string()
    }

    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().canonicalize().unwrap().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "--quiet"]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("CLAUDE.md"), "guide\n").unwrap();
    std::os::unix::fs::symlink("CLAUDE.md", repo.join("AGENTS.md")).unwrap();
    git(&repo, &["add", "CLAUDE.md", "AGENTS.md"]);
    git(&repo, &["commit", "--quiet", "-m", "initial"]);
    let revision = git(&repo, &["rev-parse", "HEAD"]);

    // The slot layout the CLI runner materializes: a standalone repository in
    // Orbit state, populated by fetch so no object arrives hard-linked.
    let checkout = repo.join(".orbit/state/source-inspections-v1/0/checkout");
    fs::create_dir_all(&checkout).unwrap();
    git(&checkout, &["init", "--quiet", "--template="]);
    git(
        &checkout,
        &[
            "-c",
            "protocol.file.allow=always",
            "fetch",
            "--quiet",
            "--no-tags",
            repo.join(".git").to_str().unwrap(),
            &revision,
        ],
    );
    git(&checkout, &["checkout", "--quiet", "--detach", &revision]);
    assert!(
        fs::symlink_metadata(checkout.join("AGENTS.md"))
            .unwrap()
            .file_type()
            .is_symlink()
    );

    let mut profile = ResolvedFsProfile {
        name: "test".to_string(),
        read: vec!["/**".to_string()],
        modify: vec![format!("{}/**", repo.display())],
    };
    append_linux_git_denies(&repo, &mut profile).unwrap();
    append_linux_git_denies(&checkout, &mut profile).unwrap();

    for denied in [
        repo.join(".git/HEAD"),
        repo.join(".git/refs/heads/protected"),
        checkout.join(".git/HEAD"),
    ] {
        assert!(
            linux_bwrap_write_grant_diagnostic(&profile, &denied)
                .unwrap()
                .is_some(),
            "{} must stay write-denied",
            denied.display()
        );
    }
    assert!(
        linux_bwrap_write_grant_diagnostic(&profile, &checkout.join("AGENTS.md"))
            .unwrap()
            .is_none(),
        "the inspected tracked symlink is not Git metadata"
    );

    let planted = repo.join(".git/orbit-source-inspections-v1/0/checkout");
    fs::create_dir_all(&planted).unwrap();
    std::os::unix::fs::symlink("CLAUDE.md", planted.join("AGENTS.md")).unwrap();
    let error = append_linux_git_denies(&repo, &mut profile).unwrap_err();
    assert!(error.to_string().contains("metadata entry"), "{error}");
}
