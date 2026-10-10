use super::*;

#[cfg(target_os = "linux")]
struct MaskFixture {
    _temp: tempfile::TempDir,
    workspace: PathBuf,
    sentinel: PathBuf,
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
        workspace,
        sentinel,
    }
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
        files: Vec::new(),
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
