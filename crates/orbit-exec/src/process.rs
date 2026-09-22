use std::process::{Child, Command, Stdio};

use orbit_common::OrbitError;

use crate::runner::{EnvironmentMode, ExecRequest, StdinMode};

/// An already-open descriptor the parent hands a child at a fixed number.
///
/// This is how a credential reaches a child that must not be able to re-open
/// it by path: the mapping is applied between `fork` and `exec`, `dup2` clears
/// close-on-exec on the target, and every descendant that does not close the
/// number inherits it. A descendant that closes it holds nothing, which is the
/// point — identity travels on the descriptor, not on the environment or the
/// process tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InheritedFd {
    /// The parent's descriptor. The caller keeps it open until `spawn`
    /// returns; `exec` closes the parent's own copy in the child.
    pub source: i32,
    /// The number the child sees it at.
    pub target: i32,
}

/// Map `fds` into the child before `exec`.
///
/// Registered as a `pre_exec` callback, so it runs after `std` has put the
/// standard streams on 0, 1 and 2. A target that collides with one of the
/// parent's own close-on-exec descriptors only closes it early, which `exec`
/// would have done anyway; a caller therefore picks a target above the
/// standard streams and keeps sources clear of it.
#[cfg(unix)]
pub(crate) fn attach_inherited_fds(command: &mut Command, fds: &[InheritedFd]) {
    use std::os::unix::process::CommandExt;

    if fds.is_empty() {
        return;
    }
    let fds = fds.to_vec();
    // SAFETY: the closure runs in the forked child before exec and issues only
    // async-signal-safe syscalls on descriptors the parent still holds open.
    unsafe {
        command.pre_exec(move || {
            for fd in &fds {
                if fd.source != fd.target && libc::dup2(fd.source, fd.target) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // `dup2` clears close-on-exec on the descriptor it creates, but
                // a source that already *is* the target keeps whatever flags it
                // had, so the flag is cleared explicitly either way.
                if libc::fcntl(fd.target, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

/// Descriptor inheritance is a Unix contract; nothing Orbit sandboxes runs
/// anywhere else.
#[cfg(not(unix))]
pub(crate) fn attach_inherited_fds(_command: &mut Command, _fds: &[InheritedFd]) {}

/// Build the child process description shared by every spawn path, so a
/// sandbox that confines the child cannot drift from the unconfined one.
pub(crate) fn command(req: &ExecRequest) -> Command {
    let mut command = Command::new(&req.program);
    command.args(&req.args).stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    if let Some(current_dir) = &req.current_dir {
        command.current_dir(current_dir);
    }

    // Make the child a process group leader (pgid = pid).  This lets us send
    // SIGKILL to the entire group after the child exits, ensuring that any
    // orphan subprocesses the agent spawned (which may have inherited the
    // stdout/stderr pipe write ends) are also killed.  Without this, those
    // orphans keep the pipes open and `wait_with_output` hangs indefinitely.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    if let EnvironmentMode::ClearAndSet(pairs) = &req.environment_mode {
        command.env_clear();
        command.envs(pairs.iter().cloned());
    }

    match req.stdin_mode {
        StdinMode::Inherit => {
            command.stdin(Stdio::inherit());
        }
        StdinMode::Null => {
            command.stdin(Stdio::null());
        }
        StdinMode::Bytes(_) => {
            command.stdin(Stdio::piped());
        }
    }

    command
}

pub(crate) fn spawn(req: &ExecRequest) -> Result<Child, OrbitError> {
    spawn_with_inherited_fds(req, &[])
}

/// Spawn `req` with no Orbit sandbox, handing the child `fds` at their fixed
/// numbers.
///
/// The unconfined counterpart of the sandboxed spawns: a plugin whose
/// manifest opted out of confinement still needs its callback credential, and
/// the credential must not depend on which boundary the host applied.
pub fn spawn_with_inherited_fds(
    req: &ExecRequest,
    fds: &[InheritedFd],
) -> Result<Child, OrbitError> {
    let mut command = command(req);
    attach_inherited_fds(&mut command, fds);
    command
        .spawn()
        .map_err(|e| OrbitError::Execution(format!("failed to spawn `{}`: {e}", req.program)))
}
