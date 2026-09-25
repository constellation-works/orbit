//! The native unit manager (launchd or systemd) and how Orbit runs its commands.

use std::process::Command;

use orbit_common::OrbitError;

use super::status::manager_probe_failure;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClockPlatform {
    Launchd,
    Systemd,
}

impl ClockPlatform {
    pub(crate) fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Launchd
        } else {
            Self::Systemd
        }
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Launchd => "launchd",
            Self::Systemd => "systemd",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManagerCommand {
    pub(super) program: &'static str,
    pub(super) args: Vec<String>,
}

impl ManagerCommand {
    pub(super) fn display(&self) -> String {
        std::iter::once(self.program.to_string())
            .chain(self.args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

pub(super) trait ClockCommandRunner {
    fn run(&self, command: &ManagerCommand) -> Result<bool, OrbitError>;
    fn stdout(&self, command: &ManagerCommand) -> Result<Option<String>, OrbitError>;

    fn probe(&self, command: &ManagerCommand) -> Result<ManagerCommandOutput, OrbitError> {
        let success = self.run(command)?;
        Ok(ManagerCommandOutput {
            success,
            exit_code: Some(if success { 0 } else { 1 }),
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManagerCommandOutput {
    pub(super) success: bool,
    pub(super) exit_code: Option<i32>,
    pub(super) stdout: String,
    pub(super) stderr: String,
}

pub(super) struct NativeClockCommandRunner;

impl ClockCommandRunner for NativeClockCommandRunner {
    fn run(&self, command: &ManagerCommand) -> Result<bool, OrbitError> {
        Command::new(command.program)
            .args(&command.args)
            .output()
            .map(|output| output.status.success())
            .map_err(|error| OrbitError::Execution(format!("run {}: {error}", command.display())))
    }

    fn stdout(&self, command: &ManagerCommand) -> Result<Option<String>, OrbitError> {
        let output = self.probe(command)?;
        if output.success {
            Ok(Some(output.stdout))
        } else {
            Err(manager_probe_failure(command, &output))
        }
    }

    fn probe(&self, command: &ManagerCommand) -> Result<ManagerCommandOutput, OrbitError> {
        Command::new(command.program)
            .args(&command.args)
            .output()
            .map(|output| ManagerCommandOutput {
                success: output.status.success(),
                exit_code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
            .map_err(|error| OrbitError::Execution(format!("run {}: {error}", command.display())))
    }
}
