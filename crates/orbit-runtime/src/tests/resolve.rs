//! Root resolution from linked worktrees must not spawn git (ORB-24406).

#[cfg(unix)]
mod git_spawns {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .output()
            .expect("run git fixture");
        assert!(
            output.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn init_main_repo(root: &Path) -> PathBuf {
        let main = root.join("main");
        std::fs::create_dir_all(&main).expect("main repo dir");
        git(&main, &["init", "--quiet"]);
        std::fs::write(main.join("file.txt"), "contents\n").expect("seed file");
        git(&main, &["add", "file.txt"]);
        git(&main, &["commit", "--quiet", "-m", "seed"]);
        main.canonicalize().expect("canonical main")
    }

    fn init_orbit_dir(dir: &Path) {
        std::fs::create_dir_all(dir).expect("orbit dir");
        std::fs::write(dir.join("config.toml"), "# test workspace\n").expect("orbit config");
    }

    /// Install a `git` shim first on PATH that appends one line per spawn
    /// to the returned count file, delegating to the real git.
    fn install_counting_git(root: &Path) -> PathBuf {
        let real_git = std::env::var_os("PATH")
            .and_then(|paths| {
                std::env::split_paths(&paths).find_map(|dir| {
                    let candidate = dir.join("git");
                    candidate.is_file().then_some(candidate)
                })
            })
            .expect("real git on PATH");
        let shim_dir = root.join("git-shim");
        std::fs::create_dir_all(&shim_dir).expect("shim dir");
        let count = root.join("git-spawns");
        std::fs::write(&count, "").expect("count file");
        let shim = format!(
            "#!/bin/sh\nprintf 'x\\n' >> '{}'\nexec '{}' \"$@\"\n",
            count.display(),
            real_git.display()
        );
        let shim_path = shim_dir.join("git");
        std::fs::write(&shim_path, shim).expect("shim");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim_path, std::fs::Permissions::from_mode(0o755))
                .expect("shim permissions");
        }
        let mut paths = vec![shim_dir];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        // SAFETY: this test runs in an isolated child process (see
        // `run_isolated_test`) executing exactly one test, so no other
        // thread observes the PATH change.
        unsafe {
            std::env::set_var("PATH", std::env::join_paths(paths).expect("join PATH"));
        }
        count
    }

    fn spawn_count(count: &Path) -> usize {
        std::fs::read_to_string(count)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    fn reset_count(count: &Path) {
        std::fs::write(count, "").expect("reset count");
    }

    #[test]
    fn linked_worktree_roots_resolve_without_spawning_git() {
        if crate::tests::run_isolated_test(std::any::type_name_of_val(
            &linked_worktree_roots_resolve_without_spawning_git,
        )) {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let main = init_main_repo(temp.path());
        init_orbit_dir(&main.join(".orbit"));
        let worktree = temp.path().join("linked");
        git(
            &main,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                &worktree.to_string_lossy(),
            ],
        );
        let deep = worktree.join("sub").join("deep");
        std::fs::create_dir_all(&deep).expect("deep dir");
        let worktree = worktree.canonicalize().expect("canonical worktree");

        // Git's own answers for the standard layout, before counting starts.
        assert_eq!(
            PathBuf::from(git(&deep, &["rev-parse", "--show-toplevel"])),
            worktree,
        );
        let common = PathBuf::from(git(
            &deep,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        ));
        assert_eq!(common.parent(), Some(main.as_path()));

        let count = install_counting_git(temp.path());

        let expected_shared = main.join(".orbit");
        let expected_local = worktree.join(".orbit");

        let roots = crate::resolve_bootstrap_roots(&deep, None).expect("bootstrap roots");
        assert_eq!(roots.shared_root, expected_shared);
        assert_eq!(roots.local_root, expected_local);
        assert_eq!(spawn_count(&count), 0, "bootstrap roots must not spawn git");
        reset_count(&count);

        let roots = crate::resolve_initialize_roots(&deep, None).expect("initialize roots");
        assert_eq!(roots.shared_root, expected_shared);
        assert_eq!(roots.local_root, expected_local);
        assert_eq!(
            spawn_count(&count),
            0,
            "initialize roots must not spawn git"
        );
    }

    #[test]
    fn ordinary_checkout_roots_still_spawn_no_git() {
        if crate::tests::run_isolated_test(std::any::type_name_of_val(
            &ordinary_checkout_roots_still_spawn_no_git,
        )) {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let main = init_main_repo(temp.path());
        let deep = main.join("sub").join("deep");
        std::fs::create_dir_all(&deep).expect("deep dir");

        let count = install_counting_git(temp.path());

        // No `.orbit/` anywhere: step 7 falls back to the git repo root
        // through the filesystem, without spawning git. The initialized
        // path is used because the bootstrap walk-up boundary still asks
        // git for an ordinary checkout's top level (pre-existing, out of
        // scope for this change).
        let expected = main.join(".orbit");
        let roots = crate::resolve_initialize_roots(&deep, None).expect("initialize roots");
        assert_eq!(roots.shared_root, expected);
        assert_eq!(roots.local_root, expected);
        assert_eq!(
            spawn_count(&count),
            0,
            "ordinary checkout must not spawn git"
        );
    }

    #[test]
    fn submodule_gitfile_still_defers_to_git() {
        if crate::tests::run_isolated_test(std::any::type_name_of_val(
            &submodule_gitfile_still_defers_to_git,
        )) {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let main = init_main_repo(temp.path());
        init_orbit_dir(&main.join(".orbit"));
        let sub_src = temp.path().join("sub-src");
        std::fs::create_dir_all(&sub_src).expect("sub src dir");
        git(&sub_src, &["init", "--quiet"]);
        std::fs::write(sub_src.join("inner.txt"), "inner\n").expect("seed file");
        git(&sub_src, &["add", "inner.txt"]);
        git(&sub_src, &["commit", "--quiet", "-m", "seed"]);
        git(
            &main,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "--quiet",
                &sub_src.to_string_lossy(),
                "sub",
            ],
        );
        git(&main, &["commit", "--quiet", "-m", "add submodule"]);
        let sub = main.join("sub");

        let count = install_counting_git(temp.path());

        // A submodule gitfile points into `modules/` and must not resolve to
        // the main checkout; git stays the authority, so git must run.
        assert_eq!(crate::paths::find_git_main_worktree_root(&sub), None);
        let worktree = crate::paths::find_git_worktree_root(&sub).expect("submodule root");
        assert_eq!(worktree, sub.canonicalize().expect("canonical sub"));
        assert!(
            spawn_count(&count) > 0,
            "submodule layout must defer to git"
        );
    }

    #[test]
    fn location_override_forces_git_authority() {
        if crate::tests::run_isolated_test(std::any::type_name_of_val(
            &location_override_forces_git_authority,
        )) {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let main = init_main_repo(temp.path());
        init_orbit_dir(&main.join(".orbit"));
        let worktree = temp.path().join("linked");
        git(
            &main,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                &worktree.to_string_lossy(),
            ],
        );
        let deep = worktree.join("sub");
        std::fs::create_dir_all(&deep).expect("sub dir");
        let worktree = worktree.canonicalize().expect("canonical worktree");

        let count = install_counting_git(temp.path());

        // `run_git` strips `GIT_DIR` before spawning, so the answer is
        // unchanged while the override still forces the git path.
        // SAFETY: isolated child process; no other thread observes the env.
        unsafe {
            std::env::set_var("GIT_DIR", "/nonexistent-git-dir-for-test");
        }
        let roots = crate::resolve_bootstrap_roots(&deep, None).expect("bootstrap roots");
        assert_eq!(roots.shared_root, main.join(".orbit"));
        assert_eq!(roots.local_root, worktree.join(".orbit"));
        assert!(
            spawn_count(&count) > 0,
            "location override must force git authority"
        );
    }
}
