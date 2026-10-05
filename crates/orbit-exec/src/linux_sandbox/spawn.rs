use super::*;

#[cfg(unix)]
pub(super) fn prepare_mount_source(source: Arc<File>) -> Result<Arc<File>, OrbitError> {
    if source.as_raw_fd() >= 3 {
        return Ok(source);
    }

    Err(OrbitError::Execution(format!(
        "Linux runtime grant descriptor {} conflicts with child standard I/O",
        source.as_raw_fd()
    )))
}

#[cfg(not(unix))]
pub(super) fn prepare_mount_source(_source: Arc<File>) -> Result<Arc<File>, OrbitError> {
    Err(OrbitError::Execution(
        "descriptor-backed Linux runtime grants require Unix file descriptors".to_string(),
    ))
}

#[cfg(unix)]
pub(super) fn inherit_mount_sources(command: &mut Command, mount_sources: &[Arc<File>]) {
    use std::os::unix::process::CommandExt;

    let source_fds = mount_sources
        .iter()
        .map(AsRawFd::as_raw_fd)
        .collect::<Vec<_>>();
    unsafe {
        command.pre_exec(move || {
            // Keep each source at its already-occupied descriptor. Remapping
            // into a conventional low range can overwrite Command's private
            // exec-error pipe, causing exec failures to look like success.
            for source in &source_fds {
                let flags = libc::fcntl(*source, libc::F_GETFD);
                if flags < 0 || libc::fcntl(*source, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

pub fn spawn_under_linux_bwrap(request: LinuxBwrapSpawnRequest<'_>) -> Result<Child, OrbitError> {
    let LinuxBwrapSpawnRequest {
        plan,
        env,
        cwd,
        stdin,
        stdout,
        stderr,
    } = request;
    if plan.wrapper != TRUSTED_BWRAP_PATH {
        return Err(OrbitError::Execution(format!(
            "refusing untrusted Bubblewrap wrapper `{}`",
            plan.wrapper
        )));
    }
    let mut command = Command::new(TRUSTED_BWRAP_PATH);
    command
        // `env` is the complete child environment the caller composed from the
        // `[execution.env]` allowlist; the sandbox adds nothing ambient of its
        // own. Bubblewrap passes its own environment through to the confined
        // program, so anything seeded here reaches the provider. [ORB-10917]
        .args(&plan.args)
        .env_clear()
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        inherit_mount_sources(&mut command, &plan.mount_sources);
        command.process_group(0);
    }
    command.spawn().map_err(|error| {
        OrbitError::Execution(format!(
            "failed to spawn trusted Bubblewrap `{TRUSTED_BWRAP_PATH}`: {error}"
        ))
    })
}
