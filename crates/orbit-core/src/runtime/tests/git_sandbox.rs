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

    use super::append_linux_git_denies;
    use orbit_types::policy::ResolvedFsProfile;

    for case in [
        "symlink-entry",
        "hardlink-entry",
        "hardlink-pointer",
        "invalid-pointer",
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

    use super::append_linux_git_denies;
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
