use super::*;

#[cfg(target_os = "linux")]
struct MaskFixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    workspace: PathBuf,
    sentinel: PathBuf,
    targets: Vec<PathBuf>,
}

#[cfg(target_os = "linux")]
fn mask_fixture() -> MaskFixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    let workspace = root.join("workspace");
    let sentinel = root.join("global/state/plugin-broker/masked");
    let targets = vec![
        root.join("global/state/plugins"),
        root.join("global/state/plugin-secrets"),
    ];
    for dir in [&workspace, &sentinel, &workspace.join("target")]
        .into_iter()
        .chain(&targets)
    {
        fs::create_dir_all(dir).expect("create dir");
    }
    fs::write(sentinel.join(".orbit-brokered"), b"masked").expect("sentinel file");
    MaskFixture {
        _temp: temp,
        root,
        workspace,
        sentinel,
        targets,
    }
}

#[cfg(target_os = "linux")]
fn mount_index(args: &[String], mode: &str, source: &Path, destination: &Path) -> Option<usize> {
    args.windows(3).position(|triple| {
        triple[0] == mode
            && triple[1] == source.display().to_string()
            && triple[2] == destination.display().to_string()
    })
}

/// The mask must come after every policy and alias mount: a later write grant
/// over a parent of a masked tree would otherwise expose the host tree again.
#[cfg(target_os = "linux")]
#[test]
fn mask_binds_the_sentinel_over_each_tree_after_every_other_mount() {
    let fixture = mask_fixture();
    let resolved = profile(vec![
        format!("{}/**", fixture.workspace.display()),
        format!("{}/global/**", fixture.root.display()),
    ]);
    let mask = LinuxBwrapMask {
        sentinel: fixture.sentinel.clone(),
        targets: fixture.targets.clone(),
    };

    let plan = compile_linux_bwrap_argv_with_authority(
        &resolved,
        "/bin/true",
        &[],
        Some(&fixture.workspace),
        true,
        Vec::new(),
        Some(&mask),
    )
    .expect("masked plan");

    let chdir = plan
        .args
        .iter()
        .position(|arg| arg == "--chdir")
        .expect("chdir");
    let last_policy_mount = plan
        .args
        .iter()
        .enumerate()
        .filter(|(_, arg)| matches!(arg.as_str(), "--bind" | "--ro-bind"))
        .map(|(index, _)| index)
        .filter(|index| {
            !fixture.targets.iter().any(|target| {
                mount_index(&plan.args, "--ro-bind", &fixture.sentinel, target) == Some(*index)
            })
        })
        .max()
        .expect("policy mounts");
    assert!(
        mount_index(
            &plan.args,
            "--bind",
            &fixture.workspace,
            Path::new(LINUX_STABLE_WORKSPACE_MOUNT)
        )
        .is_some(),
        "the managed worktree keeps its stable alias: {:?}",
        plan.args
    );
    for target in &fixture.targets {
        let masked = mount_index(&plan.args, "--ro-bind", &fixture.sentinel, target)
            .unwrap_or_else(|| panic!("no mask over {}: {:?}", target.display(), plan.args));
        assert!(
            last_policy_mount < masked && masked < chdir,
            "mask over {} must follow every policy mount and precede --chdir: {:?}",
            target.display(),
            plan.args
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn unmasked_plan_carries_no_sentinel_mount() {
    let fixture = mask_fixture();
    let resolved = profile(vec![format!("{}/**", fixture.workspace.display())]);

    let plan = compile_linux_bwrap_argv_with_authority(
        &resolved,
        "/bin/true",
        &[],
        Some(&fixture.workspace),
        true,
        Vec::new(),
        None,
    )
    .expect("plan");

    assert!(
        !plan
            .args
            .iter()
            .any(|arg| arg == &fixture.sentinel.display().to_string()),
        "{:?}",
        plan.args
    );
}

/// A tree inside the managed worktree is reachable again through the stable
/// `/tmp/orbit-workspace` alias, so one mount cannot hide it.
#[cfg(target_os = "linux")]
#[test]
fn mask_refuses_a_tree_the_plan_aliases_elsewhere() {
    let fixture = mask_fixture();
    let inside = fixture.workspace.join("state/plugins");
    fs::create_dir_all(&inside).expect("tree in worktree");
    let resolved = profile(vec![format!("{}/**", fixture.workspace.display())]);
    let mask = LinuxBwrapMask {
        sentinel: fixture.sentinel.clone(),
        targets: vec![inside],
    };

    let error = compile_linux_bwrap_argv_with_authority(
        &resolved,
        "/bin/true",
        &[],
        Some(&fixture.workspace),
        true,
        Vec::new(),
        Some(&mask),
    )
    .expect_err("aliased tree must refuse the plan");

    let OrbitError::PolicyDenied(message) = error else {
        panic!("expected a policy refusal, got {error:?}");
    };
    assert!(
        message.contains(&format!("{LINUX_STABLE_WORKSPACE_MOUNT}/state/plugins")),
        "the refusal names the alias: {message}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn mask_refuses_missing_or_overlapping_paths() {
    let fixture = mask_fixture();
    let resolved = profile(vec![format!("{}/**", fixture.workspace.display())]);
    let compile = |mask: LinuxBwrapMask| {
        compile_linux_bwrap_argv_with_authority(
            &resolved,
            "/bin/true",
            &[],
            Some(&fixture.workspace),
            false,
            Vec::new(),
            Some(&mask),
        )
    };

    assert!(
        compile(LinuxBwrapMask {
            sentinel: fixture.sentinel.clone(),
            targets: vec![fixture.root.join("global/state/missing")],
        })
        .is_err(),
        "a missing target cannot be mounted over"
    );
    assert!(
        compile(LinuxBwrapMask {
            sentinel: fixture.root.join("global/missing-sentinel"),
            targets: fixture.targets.clone(),
        })
        .is_err(),
        "a missing sentinel cannot be mounted"
    );
    assert!(
        compile(LinuxBwrapMask {
            sentinel: fixture.sentinel.clone(),
            targets: vec![fixture.root.join("global/state")],
        })
        .is_err(),
        "a target holding the sentinel would mount the sentinel over itself"
    );
}

#[test]
fn plan_alias_maps_a_target_through_a_renaming_bind() {
    let args: Vec<String> = [
        "--ro-bind",
        "/",
        "/",
        "--bind",
        "/work",
        "/work",
        "--bind",
        "/work",
        "/tmp/orbit-workspace",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();

    assert_eq!(
        plan_alias(&args, Path::new("/work/state/plugins")),
        Some(PathBuf::from("/tmp/orbit-workspace/state/plugins"))
    );
    assert_eq!(
        plan_alias(&args, Path::new("/work")),
        Some(PathBuf::from("/tmp/orbit-workspace"))
    );
    assert_eq!(
        plan_alias(&args, Path::new("/srv/orbit/state/plugins")),
        None
    );
}

#[test]
fn parse_mountinfo_decodes_octal_escapes() {
    let mounts = parse_mountinfo(
        "36 35 98:0 /mnt\\040dir /data\\011x rw,noatime master:1 - ext3 /dev/root rw\n\
         37 35 98:1 / / rw - ext4 /dev/sda1 rw\n",
    );

    assert_eq!(
        mounts,
        vec![
            MountEntry {
                device: "98:0".to_string(),
                root: PathBuf::from("/mnt dir"),
                mount_point: PathBuf::from("/data\tx"),
            },
            MountEntry {
                device: "98:1".to_string(),
                root: PathBuf::from("/"),
                mount_point: PathBuf::from("/"),
            },
        ]
    );
}

fn entry(device: &str, root: &Path, mount_point: &Path) -> MountEntry {
    MountEntry {
        device: device.to_string(),
        root: root.to_path_buf(),
        mount_point: mount_point.to_path_buf(),
    }
}

/// A second mount of the target's filesystem whose root holds the target
/// shows the target again; the symlink stands in for the second mount point
/// so the device-and-inode confirmation sees the same directory.
#[cfg(unix)]
#[test]
fn host_alias_reports_a_second_mount_that_holds_the_target() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    let target = root.join("real/state/plugins");
    fs::create_dir_all(&target).expect("target");
    std::os::unix::fs::symlink(root.join("real"), root.join("alias")).expect("alias");

    let mounts = [
        entry("8:1", Path::new("/"), Path::new("/")),
        entry("8:1", &root.join("real"), &root.join("alias")),
    ];

    assert_eq!(
        host_alias(&mounts, &target).expect("alias check"),
        Some(root.join("alias/state/plugins"))
    );
}

#[cfg(unix)]
#[test]
fn host_alias_reports_a_second_mount_of_part_of_the_target() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    let target = root.join("state/plugins");
    fs::create_dir_all(target.join("demo")).expect("target");
    std::os::unix::fs::symlink(target.join("demo"), root.join("exposed")).expect("alias");

    let mounts = [
        entry("8:1", Path::new("/"), Path::new("/")),
        entry("8:1", &target.join("demo"), &root.join("exposed")),
    ];

    assert_eq!(
        host_alias(&mounts, &target).expect("alias check"),
        Some(root.join("exposed"))
    );
}

#[cfg(unix)]
#[test]
fn host_alias_ignores_unrelated_shadowed_and_nested_mounts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    let target = root.join("real/state/plugins");
    fs::create_dir_all(target.join("nested")).expect("target");
    fs::create_dir_all(root.join("shadowed")).expect("shadowed mount point");
    std::os::unix::fs::symlink(root.join("real"), root.join("other-device")).expect("link");

    let mounts = [
        entry("8:1", Path::new("/"), Path::new("/")),
        // Another filesystem entirely.
        entry("9:4", &root.join("real"), &root.join("other-device")),
        // Same filesystem, but a later mount hides what it would show.
        entry("8:1", &root.join("real"), &root.join("shadowed")),
        // Beneath the target: the mask hides it with the rest of the tree.
        entry("8:1", &root.join("real"), &target.join("nested")),
    ];

    assert_eq!(host_alias(&mounts, &target).expect("alias check"), None);
}
