use super::*;

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
fn descriptor_mount_plan_shares_the_validated_authority_without_duplication() {
    use super::{LinuxBwrapMountAuthority, compile_linux_bwrap_argv_with_authority};

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    let target = root.join("orbit.db-wal");
    fs::write(&target, b"validated").expect("sidecar");
    let source = std::sync::Arc::new(fs::File::open(&target).expect("open authority"));
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
            source: std::sync::Arc::clone(&source),
        }],
    )
    .expect("descriptor-backed plan");
    let retained_fd = plan.mount_sources[0].as_raw_fd();
    assert_eq!(
        retained_fd, source_fd,
        "compilation must not create a parent-side duplicate descriptor"
    );
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
    assert!(
        unsafe { libc::fcntl(source_fd, libc::F_GETFD) } >= 0,
        "dropping a mount plan must not close the runtime owner's authority"
    );

    drop(source);
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
            source: std::sync::Arc::new(source),
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
            source: std::sync::Arc::new(source),
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
            source: std::sync::Arc::new(source),
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
