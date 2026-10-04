use super::*;

#[cfg(target_os = "linux")]
#[test]
fn descriptor_inheritance_preserves_the_command_exec_error_pipe() {
    use std::process::{Command, Stdio};

    use super::{
        LinuxBwrapMountAuthority, compile_linux_bwrap_argv_with_authority, inherit_mount_sources,
    };

    const ISOLATED_CHILD: &str = "ORBIT_DESCRIPTOR_EXEC_ERROR_CHILD";
    if std::env::var_os(ISOLATED_CHILD).is_none() {
        const TEST: &str = "linux_sandbox::tests::descriptor::descriptor_inheritance_preserves_the_command_exec_error_pipe";
        let output = Command::new(std::env::current_exe().expect("current test executable"))
            .args(["--exact", TEST])
            .env(ISOLATED_CHILD, "1")
            .output()
            .expect("spawn isolated descriptor test");
        orbit_common::test_env::assert_child_test_passed(
            TEST,
            output.status,
            &output.stdout,
            &output.stderr,
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
        None,
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
        None,
    )
    .expect_err("replacement must fail closed");

    assert!(matches!(error, OrbitError::PolicyDenied(_)));
    assert_eq!(fs::read(&secret).expect("outside content"), b"outside");
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
        None,
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
