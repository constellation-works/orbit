//! Sibling tests for `tool_exec.rs` (migrated per ORB-00246 / docs/design-patterns/test_layout.md).

/// Security invariant: nested persisted identities must not compare scrubbed
/// values, while scrubbed diagnostic prose remains valid output. Crafted
/// nested replies exercise fields unavailable from the read-only probe.
#[test]
fn corrupted_drain_identities_are_detected_in_nested_replies() {
    use crate::tool_exec::corrupted_drain_identity;
    use serde_json::json;

    for field in [
        "protocol_fingerprint",
        "caller_fingerprint",
        "commit",
        "tree",
        "sha256",
        "reviewed_head_sha",
        "reviewed_base_sha",
        "reviewer_commit",
        "covering_commit",
        "final_candidate_tree",
        "head_sha",
        "task_spec_digest",
    ] {
        let reply = json!({"receipt": [{"candidate": {field: "ab[REDACTED_ENV]cd"}}]});
        assert_eq!(
            corrupted_drain_identity(&reply),
            Some(field),
            "protect {field}"
        );
    }
    let reply = json!({"receipt": {"examined_commits": ["intact", "ab[REDACTED_ENV]cd"]}});
    assert_eq!(corrupted_drain_identity(&reply), Some("examined_commits"));
    assert_eq!(
        corrupted_drain_identity(&json!({"diagnostics": ["[REDACTED_ENV]"],
        "candidate": {"commit": "a".repeat(40), "tree": "b".repeat(40)}})),
        None
    );
}

#[cfg(unix)]
mod checkout_root {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use orbit_tools::ToolContext;

    use crate::tool_exec::active_git_checkout_root;

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
        #[cfg(unix)]
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

    fn context_at(cwd: &Path) -> ToolContext {
        ToolContext {
            cwd: Some(cwd.to_string_lossy().into_owned()),
            ..ToolContext::default()
        }
    }

    #[test]
    fn linked_worktree_checkout_resolves_without_spawning_git() {
        if crate::tests::run_isolated_test(std::any::type_name_of_val(
            &linked_worktree_checkout_resolves_without_spawning_git,
        )) {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let main = init_main_repo(temp.path());
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
        // Managed layout: a linked worktree nested inside the main checkout.
        let managed = main.join(".orbit/state/worktrees/managed-one");
        git(
            &main,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                &managed.to_string_lossy(),
            ],
        );
        let count = install_counting_git(temp.path());

        let resolved = active_git_checkout_root(&main, &context_at(&deep));
        assert_eq!(
            resolved,
            Some(worktree.canonicalize().expect("canonical worktree"))
        );
        assert_eq!(
            active_git_checkout_root(&main, &context_at(&managed)),
            Some(managed.canonicalize().expect("canonical managed worktree"))
        );
        assert_eq!(spawn_count(&count), 0, "linked worktree must not spawn git");
    }

    #[test]
    fn source_inspection_checkout_second_call_spawns_no_git() {
        if crate::tests::run_isolated_test(std::any::type_name_of_val(
            &source_inspection_checkout_second_call_spawns_no_git,
        )) {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let main = init_main_repo(temp.path());
        let revision = git(&main, &["rev-parse", "HEAD"]);
        let common = main.join(".git");
        let slot = main
            .join(".orbit/state/source-inspections-v1/0")
            .to_path_buf();
        let checkout = slot.join("checkout");
        std::fs::create_dir_all(&checkout).expect("slot checkout dir");
        std::fs::write(slot.join("owner"), "orbit-source-inspection-v1\n").expect("owner marker");
        git(&checkout, &["init", "--quiet"]);
        git(
            &checkout,
            &[
                "-c",
                "protocol.file.allow=always",
                "fetch",
                "--quiet",
                "--no-tags",
                &common.to_string_lossy(),
                &revision,
            ],
        );
        git(&checkout, &["checkout", "--quiet", "--detach", &revision]);
        let count = install_counting_git(temp.path());

        let expected = Some(checkout.canonicalize().expect("canonical checkout"));
        assert_eq!(
            active_git_checkout_root(&main, &context_at(&checkout)),
            expected
        );
        std::fs::write(&count, "").expect("reset count");
        assert_eq!(
            active_git_checkout_root(&main, &context_at(&checkout)),
            expected
        );
        assert_eq!(
            spawn_count(&count),
            0,
            "memoised inspection-slot resolution must not spawn git"
        );
    }
}
