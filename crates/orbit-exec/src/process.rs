use std::process::{Child, Command, Stdio};

use orbit_common::OrbitError;

use crate::runner::{EnvironmentMode, ExecRequest, StdinMode};

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
    command(req)
        .spawn()
        .map_err(|e| OrbitError::Execution(format!("failed to spawn `{}`: {e}", req.program)))
}
