/// Pathname sockets have to fit in `sockaddr_un.sun_path`. A managed run's
/// `TMPDIR` is already longer than that limit, so these fixtures relocate
/// to a short directory instead of using the process temp dir. This is the
/// socket path, not a fallback that hides ancestor-config discovery.
fn short_socket_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ogs")
        .rand_bytes(4)
        .tempdir_in("/tmp")
        .expect("short directory for a Unix socket path")
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
        let temp = short_socket_dir();
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
        let temp = short_socket_dir();
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
