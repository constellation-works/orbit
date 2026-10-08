//! Claim-gated remote command execution [ADR-0351, ORB-10711].
//!
//! The shared entry point every submission surface (MCP `orbit.command.exec`,
//! and any future in-process caller) funnels through, mirroring
//! [`OrbitRuntime::submit_ship_run`]'s split between a thin tool-dispatch
//! wrapper and the runtime method that owns the actual gate. `argv` is spawned
//! directly via [`std::process::Command`] — no shell — so quoting and
//! operator-precedence bugs cannot occur by construction, not by review.
//! The child runs through [`orbit_common::process::run_bounded_capped_typed`]:
//! it leads its own process group, each output stream is capped at
//! [`OUTPUT_LIMIT_BYTES`], and it is killed with its whole group when the
//! deadline passes. The deadline is [`DEFAULT_TIMEOUT_MS`] unless the caller
//! passes `timeout_ms`, which is clamped to [`MAX_TIMEOUT_MS`]. A killed
//! command is reported as `timed_out: true` with no exit code; the output it
//! had produced is discarded.
//!
//! Operator capability is enforced uniformly across every entry point by the
//! ORB-10453 governed-operation chokepoint before this method is ever reached;
//! this method owns the rest of the gate the capability check cannot see —
//! the workspace claim ([`OrbitRuntime::require_workspace_claim`]), the
//! working-directory confinement shared with `orbit.agent.invoke`, and the
//! audit record naming what actually ran.

use std::process::Command;
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_common::process::{BoundedRunError, run_bounded_capped_typed};
use orbit_common::security::redaction::{argv_redactor, is_sensitive_env_name};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::ExecutionResult;
use serde_json::json;

use super::audit::coordination::{CoordinationAuditEvent, record_coordination_audit_event};
use crate::OrbitRuntime;

const COMMAND_TOOL_NAME: &str = "orbit.command.exec";
const COMMAND_TARGET_TYPE: &str = "command_exec";

/// Deadline when the caller passes no `timeout_ms`.
pub(crate) const DEFAULT_TIMEOUT_MS: u64 = 120_000;
/// Largest deadline a caller may request; larger values are clamped to it.
pub(crate) const MAX_TIMEOUT_MS: u64 = 600_000;
/// Bytes kept from each of stdout and stderr; the rest is read and dropped.
const OUTPUT_LIMIT_BYTES: usize = 4 * 1024 * 1024;

/// Parameters for [`OrbitRuntime::execute_remote_command`], already parsed and
/// typed by the tool-dispatch layer.
pub(crate) struct RemoteCommandParams {
    pub(crate) argv: Vec<String>,
    pub(crate) working_directory: String,
    pub(crate) claim_token: Option<String>,
    pub(crate) actor: String,
    /// Caller-requested deadline; `None` means [`DEFAULT_TIMEOUT_MS`].
    pub(crate) timeout_ms: Option<u64>,
}

impl OrbitRuntime {
    /// Run `params.argv` in `params.working_directory` after the workspace
    /// claim admits the caller, and audit the attempt regardless of outcome.
    ///
    /// `working_directory` is confined by `resolve_workspace_cwd`, the same
    /// helper `orbit.agent.invoke` uses for `cwd`.
    pub(crate) fn execute_remote_command(
        &self,
        params: RemoteCommandParams,
    ) -> Result<ExecutionResult, OrbitError> {
        self.require_workspace_claim(COMMAND_TOOL_NAME, params.claim_token.as_deref())?;
        let working_directory =
            self.resolve_workspace_cwd("working_directory", &params.working_directory)?;

        let mut argv = params.argv.into_iter();
        let program = argv
            .next()
            .ok_or_else(|| OrbitError::InvalidInput("`argv` must not be empty".to_string()))?;
        let args: Vec<String> = argv.collect();
        let full_argv: Vec<String> = std::iter::once(program.clone())
            .chain(args.clone())
            .collect();

        let env_pairs = std::env::vars().filter(|(key, _)| !is_sensitive_env_name(key));

        let timeout_ms = params
            .timeout_ms
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .clamp(1, MAX_TIMEOUT_MS);
        let mut command = Command::new(&program);
        command
            .args(&args)
            .current_dir(&working_directory)
            .env_clear()
            .envs(env_pairs);

        let started = Instant::now();
        let result = match run_bounded_capped_typed(
            &mut command,
            Duration::from_millis(timeout_ms),
            OUTPUT_LIMIT_BYTES,
        ) {
            Ok(output) => Ok(ExecutionResult {
                success: output.status.success(),
                timed_out: false,
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                exit_code: output.status.code(),
                duration_ms: started.elapsed().as_millis() as u64,
                output: None,
            }),
            Err(BoundedRunError::Run(OrbitError::ProcessTimeout { .. })) => Ok(ExecutionResult {
                success: false,
                timed_out: true,
                stdout: String::new(),
                stderr: format!(
                    "'{program}' did not finish within {timeout_ms}ms; it and its process \
                     group were killed"
                ),
                exit_code: None,
                duration_ms: started.elapsed().as_millis() as u64,
                output: None,
            }),
            Err(BoundedRunError::Spawn(error)) => Err(OrbitError::Execution(format!(
                "spawn '{program}' in '{}': {error}",
                working_directory.display()
            ))),
            Err(BoundedRunError::Run(error)) => Err(OrbitError::Execution(format!(
                "run '{program}' in '{}': {error}",
                working_directory.display()
            ))),
        };

        let status = if result.as_ref().is_ok_and(|result| !result.timed_out) {
            AuditEventStatus::Success
        } else {
            AuditEventStatus::Failure
        };
        let redaction = argv_redactor();
        let argv_redacted: Vec<String> = full_argv
            .iter()
            .map(|arg| redaction.apply_str(arg))
            .collect();
        if let Err(audit_error) = record_coordination_audit_event(
            self,
            CoordinationAuditEvent {
                command: "command.exec",
                tool_name: COMMAND_TOOL_NAME,
                target_type: COMMAND_TARGET_TYPE,
                target_id: None,
                task_id: None,
                status,
                payload: json!({
                    "argv": argv_redacted,
                    "working_directory": working_directory.display().to_string(),
                    "caller": params.actor,
                    "workspace": self.paths().repo_root.to_string_lossy(),
                }),
            },
        ) {
            tracing::error!(
                target: "orbit.command.exec",
                error = %audit_error,
                "failed to persist command execution audit event"
            );
        }

        result
    }
}
