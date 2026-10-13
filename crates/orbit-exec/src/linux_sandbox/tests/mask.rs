//! Security invariant: injected mount tables exercise alias identity checks
//! without privileged host mounts; the public compiler reads /proc mountinfo.
use super::*;

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
