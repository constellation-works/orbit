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
