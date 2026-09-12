//! Unit tests for `linux_sandbox` rule expansion — sibling layout under
//! src/tests/. The filesystem walk is platform-neutral, so these run
//! everywhere; the bwrap spawn itself is covered by the Linux-only
//! integration tests.

use std::collections::BTreeSet;
use std::fs;
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::PathBuf;

use super::{LinuxBwrapPostRunGuard, expand_rule, expand_rules, walk_paths};
use orbit_common::OrbitError;
use orbit_types::policy::ResolvedFsProfile;

fn tree() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    for dir in ["a", "a/deep", "b", "target/debug"] {
        fs::create_dir_all(root.join(dir)).expect("create dir");
    }
    for file in [
        ".env",
        "a/.env",
        "a/deep/.env.local",
        "b/settings.env",
        "b/env.txt",
        "target/debug/build.env.bak",
    ] {
        fs::write(root.join(file), b"x").expect("write file");
    }
    temp
}

fn canonical(root: &std::path::Path, rel: &str) -> PathBuf {
    root.join(rel).canonicalize().expect("canonical")
}

/// Every rule sharing a search root is matched from one walk, and the union
/// equals what the rules would have matched one at a time.
#[test]
fn rules_sharing_a_root_expand_from_one_walk_to_the_same_set() {
    let temp = tree();
    let root = temp.path().canonicalize().expect("canonical root");
    let prefix = root.to_string_lossy().replace('\\', "/");
    let rules: Vec<String> = ["**/.env", "**/.env.*", "**/*.env", "**/*.env.*"]
        .iter()
        .map(|glob| format!("{prefix}/{glob}"))
        .collect();

    let together = expand_rules(&rules).expect("expand together");
    let mut one_at_a_time = BTreeSet::new();
    for rule in &rules {
        one_at_a_time.extend(expand_rule(rule).expect("expand one"));
    }
    assert_eq!(together, one_at_a_time);

    let expected: BTreeSet<PathBuf> = [
        ".env",
        "a/.env",
        "a/deep/.env.local",
        "b/settings.env",
        "target/debug/build.env.bak",
    ]
    .iter()
    .map(|rel| canonical(&root, rel))
    .collect();
    assert_eq!(together, expected);
}

/// The walk lists each path exactly once: directories used to be pushed on
/// entry and again from their parent's listing.
#[test]
fn walk_lists_every_path_once() {
    let temp = tree();
    let root = temp.path().canonicalize().expect("canonical root");
    let mut paths = Vec::new();
    walk_paths(&root, &mut paths).expect("walk");
    let unique: BTreeSet<&PathBuf> = paths.iter().collect();
    assert_eq!(unique.len(), paths.len(), "duplicates in {paths:?}");
    // 1 root + 4 dirs (a, a/deep, b, target, target/debug = 5) + 6 files.
    assert_eq!(paths.len(), 1 + 5 + 6);
    assert_eq!(paths[0], root);
}

fn profile(modify: Vec<String>) -> ResolvedFsProfile {
    ResolvedFsProfile {
        name: "test".to_string(),
        read: vec!["/**".to_string()],
        modify,
    }
}

#[test]
fn capture_watches_absent_exact_and_subtree_denies() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let secrets = workspace.join("secrets");
    let lock = workspace.join("Cargo.lock");
    let resolved = profile(vec![
        format!("{}/**", workspace.display()),
        format!("!{}/**", secrets.display()),
        format!("!{}", lock.display()),
    ]);

    let guard = LinuxBwrapPostRunGuard::capture(&resolved)
        .expect("capture")
        .expect("absent exact/subtree denies must be guarded");
    fs::create_dir_all(&secrets).expect("create secrets");
    fs::write(secrets.join("x"), b"k").expect("write secret");
    fs::write(&lock, b"k").expect("write lock");

    let error = guard
        .verify()
        .expect_err("creating an absent deny root must fail closed");
    assert!(
        matches!(error, OrbitError::PolicyDenied(_)),
        "expected PolicyDenied, got {error}"
    );
}

/// macOS commonly reaches `/private/var` through the `/var` symlink. The
/// guard must match rules written through that spelling even though its walk
/// canonicalizes the search root.
#[cfg(unix)]
#[test]
fn capture_watches_absent_denies_through_a_symlinked_workspace_path() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let real_workspace = temp.path().join("real-workspace");
    let workspace = temp.path().join("workspace-link");
    fs::create_dir_all(&real_workspace).expect("real workspace");
    symlink(&real_workspace, &workspace).expect("workspace symlink");

    let secrets = workspace.join("secrets");
    let lock = workspace.join("Cargo.lock");
    let resolved = profile(vec![
        format!("{}/**", workspace.display()),
        format!("!{}/**", secrets.display()),
        format!("!{}", lock.display()),
    ]);

    let guard = LinuxBwrapPostRunGuard::capture(&resolved)
        .expect("capture")
        .expect("absent exact/subtree denies must be guarded");
    fs::create_dir_all(&secrets).expect("create secrets");
    fs::write(secrets.join("x"), b"k").expect("write secret");
    fs::write(&lock, b"k").expect("write lock");

    let error = guard
        .verify()
        .expect_err("creating a deny root through a symlink must fail closed");
    assert!(
        matches!(error, OrbitError::PolicyDenied(_)),
        "expected PolicyDenied, got {error}"
    );
}

#[test]
fn capture_skips_absent_deny_whose_nested_reallow_will_create_the_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let orbit = workspace.join(".orbit");
    let resolved = profile(vec![
        format!("{}/**", workspace.display()),
        format!("!{}/**", orbit.display()),
        format!("{}/**", orbit.join("auto_tasks").display()),
    ]);

    assert!(
        LinuxBwrapPostRunGuard::capture(&resolved)
            .expect("capture")
            .is_none(),
        "grant preparation will create .orbit, so watching it would false-positive"
    );
}

#[test]
fn managed_aliases_replay_denies_and_pin_replaceable_parents() {
    use super::{LINUX_STABLE_BUILD_MOUNT, LINUX_STABLE_WORKSPACE_MOUNT, compile_linux_bwrap_argv};

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let protected = root.join("target/nested/metadata");
    fs::create_dir_all(&protected).unwrap();
    fs::write(root.join(".git"), "gitdir: target/nested/metadata").unwrap();
    let resolved = profile(vec![
        format!("{}/**", root.display()),
        format!("!{}", root.join(".git").display()),
        format!("!{}/**", protected.display()),
    ]);
    let plan = compile_linux_bwrap_argv(&resolved, "/bin/true", &[], Some(&root), true).unwrap();
    let mounts: Vec<_> = plan
        .args
        .windows(3)
        .filter(|args| matches!(args[0].as_str(), "--bind" | "--ro-bind"))
        .collect();
    for (source, destination, mode) in [
        (
            root.join(".git"),
            PathBuf::from(LINUX_STABLE_WORKSPACE_MOUNT).join(".git"),
            "--ro-bind",
        ),
        (
            protected.clone(),
            PathBuf::from(LINUX_STABLE_BUILD_MOUNT).join("nested/metadata"),
            "--ro-bind",
        ),
        (
            root.join("target/nested"),
            root.join("target/nested"),
            "--bind",
        ),
        (
            root.join("target/nested"),
            PathBuf::from(LINUX_STABLE_BUILD_MOUNT).join("nested"),
            "--bind",
        ),
    ] {
        let final_mount = mounts
            .iter()
            .rfind(|args| args[2] == destination.display().to_string())
            .unwrap();
        assert_eq!(final_mount[0], mode);
        assert_eq!(final_mount[1], source.display().to_string());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn descriptor_mount_plan_holds_the_validated_object_and_closes_it_on_drop() {
    use super::{LinuxBwrapMountAuthority, compile_linux_bwrap_argv_with_authority};

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    let target = root.join("orbit.db-wal");
    fs::write(&target, b"validated").expect("sidecar");
    let source = fs::File::open(&target).expect("open authority");
    let source_fd = source.as_raw_fd();
    fs::rename(&target, root.join("validated-sidecar")).expect("replace name");
    fs::write(&target, b"replacement").expect("replacement");
    let resolved = profile(vec![target.display().to_string()]);

    let plan = compile_linux_bwrap_argv_with_authority(
        &resolved,
        "/bin/true",
        &[],
        Some(&root),
        false,
        vec![LinuxBwrapMountAuthority {
            destination: target.clone(),
            source,
        }],
    )
    .expect("descriptor-backed plan");
    let retained_fd = plan.mount_sources[0].as_raw_fd();
    let evidence = &plan.mount_evidence()[0];
    let metadata = plan.mount_sources[0]
        .metadata()
        .expect("mounted object metadata");
    use std::os::unix::fs::MetadataExt;
    assert_eq!(evidence.destination, target);
    assert_eq!(evidence.source_fd, retained_fd);
    assert_eq!(evidence.device, metadata.dev());
    assert_eq!(evidence.inode, metadata.ino());
    assert!(plan.args.windows(3).any(|args| {
        args[0] == "--bind-fd"
            && args[1] == retained_fd.to_string()
            && args[2] == target.display().to_string()
    }));
    assert!(unsafe { libc::fcntl(source_fd, libc::F_GETFD) } >= 0);

    drop(plan);
    assert_eq!(unsafe { libc::fcntl(source_fd, libc::F_GETFD) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EBADF)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn descriptor_inheritance_preserves_the_command_exec_error_pipe() {
    use std::process::{Command, Stdio};

    use super::{
        LinuxBwrapMountAuthority, compile_linux_bwrap_argv_with_authority, inherit_mount_sources,
    };

    const ISOLATED_CHILD: &str = "ORBIT_DESCRIPTOR_EXEC_ERROR_CHILD";
    if std::env::var_os(ISOLATED_CHILD).is_none() {
        let status = Command::new(std::env::current_exe().expect("current test executable"))
            .arg("descriptor_inheritance_preserves_the_command_exec_error_pipe")
            .env(ISOLATED_CHILD, "1")
            .status()
            .expect("spawn isolated descriptor test");
        assert!(
            status.success(),
            "isolated descriptor test failed: {status}"
        );
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    let fillers = (0..16)
        .map(|_| fs::File::open("/dev/null").expect("open filler descriptor"))
        .collect::<Vec<_>>();
    let highest_filler = fillers
        .iter()
        .map(AsRawFd::as_raw_fd)
        .max()
        .expect("filler descriptor");

    let mut modify = Vec::new();
    let mut authority = Vec::new();
    for index in 0..8 {
        let destination = root.join(format!("authority-{index}"));
        fs::write(&destination, b"authority").expect("write authority fixture");
        let source = fs::File::open(&destination).expect("open authority source");
        assert!(
            source.as_raw_fd() > highest_filler,
            "authority source must be opened above the occupied low range"
        );
        modify.push(destination.display().to_string());
        authority.push(LinuxBwrapMountAuthority {
            destination,
            source,
        });
    }

    let plan = compile_linux_bwrap_argv_with_authority(
        &profile(modify),
        "/bin/true",
        &[],
        Some(&root),
        false,
        authority,
    )
    .expect("compile descriptor-backed plan");
    let inherited_fds = plan
        .mount_sources
        .iter()
        .map(AsRawFd::as_raw_fd)
        .collect::<Vec<_>>();
    let compiled_fds = plan
        .args
        .windows(2)
        .filter(|args| args[0] == "--bind-fd")
        .map(|args| args[1].parse::<i32>().expect("numeric bind descriptor"))
        .collect::<Vec<_>>();
    assert_eq!(compiled_fds, inherited_fds);

    drop(fillers);
    let mut command = Command::new("/orbit/nonexistent/bwrap");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    inherit_mount_sources(&mut command, &plan.mount_sources);

    let error = command
        .spawn()
        .expect_err("a nonexistent wrapper must return a spawn error");
    assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
}

#[cfg(target_os = "linux")]
#[test]
fn descriptor_mount_plan_rejects_an_external_symlink_replacement() {
    use std::os::unix::fs::symlink;

    use super::{LinuxBwrapMountAuthority, compile_linux_bwrap_argv_with_authority};

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&root).expect("root");
    fs::create_dir_all(&outside).expect("outside");
    let target = root.join("orbit.db-wal");
    let secret = outside.join("secret");
    fs::write(&target, b"validated").expect("sidecar");
    fs::write(&secret, b"outside").expect("secret");
    let source = fs::File::open(&target).expect("open authority");
    fs::remove_file(&target).expect("remove validated name");
    symlink(&secret, &target).expect("external replacement");
    let resolved = profile(vec![target.display().to_string()]);

    let error = compile_linux_bwrap_argv_with_authority(
        &resolved,
        "/bin/true",
        &[],
        Some(&root),
        false,
        vec![LinuxBwrapMountAuthority {
            destination: target,
            source,
        }],
    )
    .expect_err("replacement must fail closed");

    assert!(matches!(error, OrbitError::PolicyDenied(_)));
    assert_eq!(fs::read(&secret).expect("outside content"), b"outside");
}

/// Control for the original fault: path-only compilation follows the name at
/// consumption time. Keep this executable finding beside the descriptor-safe
/// case so the latter cannot become a model-only assertion.
#[cfg(target_os = "linux")]
#[test]
fn path_only_mount_plan_follows_an_external_sidecar_replacement() {
    use std::os::unix::fs::symlink;

    use super::compile_linux_bwrap_argv;

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&root).expect("root");
    fs::create_dir_all(&outside).expect("outside");
    let target = root.join("orbit.db-wal");
    let secret = outside.join("secret");
    fs::write(&target, b"validated").expect("sidecar");
    fs::write(&secret, b"outside").expect("secret");
    fs::remove_file(&target).expect("remove validated name");
    symlink(&secret, &target).expect("external replacement");
    let resolved = profile(vec![target.display().to_string()]);

    let plan = compile_linux_bwrap_argv(&resolved, "/bin/true", &[], Some(&root), false)
        .expect("path-only control compiles");
    let outside = secret.canonicalize().expect("canonical outside");

    assert!(plan.args.windows(3).any(|args| {
        args[0] == "--bind"
            && args[1] == outside.display().to_string()
            && args[2] == outside.display().to_string()
    }));
}

#[cfg(target_os = "linux")]
#[test]
fn descriptor_directory_mount_rejects_an_external_symlink_replacement() {
    use std::os::unix::fs::symlink;

    use super::{LinuxBwrapMountAuthority, compile_linux_bwrap_argv_with_authority};

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("root");
    let outside = temp.path().join("outside");
    let target = root.join("state/logs");
    fs::create_dir_all(&target).expect("runtime directory");
    fs::create_dir_all(&outside).expect("outside");
    let source = fs::File::open(&target).expect("open directory authority");
    fs::remove_dir(&target).expect("remove validated directory name");
    symlink(&outside, &target).expect("external replacement");
    let resolved = profile(vec![format!("{}/**", target.display())]);

    let error = compile_linux_bwrap_argv_with_authority(
        &resolved,
        "/bin/true",
        &[],
        Some(&root),
        false,
        vec![LinuxBwrapMountAuthority {
            destination: target,
            source,
        }],
    )
    .expect_err("directory replacement must fail closed");

    assert!(matches!(error, OrbitError::PolicyDenied(_)));
    assert!(
        fs::read_dir(&outside)
            .expect("outside directory")
            .next()
            .is_none()
    );
}
