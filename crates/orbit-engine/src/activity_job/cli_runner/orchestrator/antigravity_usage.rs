//! Read agy's own quota command on the host, including its keychain login.

use std::io::Read;
use std::path::PathBuf;

use orbit_exec::{
    EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process_streaming_stdout,
};

const MAX_BYTES: u64 = 64 * 1024;

/// The actual provider launcher and environment, resolved before dispatch.
pub(super) struct AntigravityUsageRequest {
    pub(super) program: String,
    pub(super) environment: Vec<(String, String)>,
    pub(super) cwd: Option<PathBuf>,
}

impl AntigravityUsageRequest {
    /// A fixed built-in command, never a model prompt. agy handles its existing
    /// login itself; Orbit neither extracts credentials nor adds new secrets.
    pub(super) fn read(&self) -> Option<Vec<u8>> {
        let request = ExecRequest {
            program: self.program.clone(),
            args: [
                "--print",
                "/usage",
                "--output-format",
                "json",
                "--print-timeout",
                "3s",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            current_dir: self.cwd.as_ref().map(|cwd| cwd.display().to_string()),
            timeout_ms: Some(5_000),
            stdin_mode: StdinMode::Null,
            environment_mode: EnvironmentMode::ClearAndSet(self.environment.clone()),
            debug: false,
        };
        let (result, body) = run_process_streaming_stdout(&request, &NoSandbox, |stdout| {
            let mut body = Vec::new();
            stdout
                .take(MAX_BYTES + 1)
                .read_to_end(&mut body)
                .map_err(|_| {
                    orbit_common::OrbitError::Execution("read Antigravity usage".to_string())
                })?;
            Ok(body)
        })
        .ok()?;
        (result.success && body.len() as u64 <= MAX_BYTES).then_some(body)
    }
}
