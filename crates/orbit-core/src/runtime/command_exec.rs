//! Claim-gated remote command execution [ADR-0351, ORB-10711].
//!
//! The shared entry point every submission surface (MCP `orbit.command.exec`,
//! and any future in-process caller) funnels through, mirroring
//! [`OrbitRuntime::submit_ship_run`]'s split between a thin tool-dispatch
//! wrapper and the runtime method that owns the actual gate. `argv` is spawned
//! directly via [`std::process::Command`] — no shell — so quoting and
//! operator-precedence bugs cannot occur by construction, not by review.
//! `orbit-core` spawns the child itself rather than reusing `orbit-exec`'s
//! `run_process`: that crate is not a declared dependency of `orbit-core`
//! ([`ARCHITECTURE.md`](../../../../../ARCHITECTURE.md) draws that edge only
//! from `orbit-tools` and `orbit-engine`), and the one property it adds beyond
//! `Command::output` — timeout supervision — is not part of this operation's
//! contract.
//!
//! Operator capability is enforced uniformly across every entry point by the
//! ORB-10453 governed-operation chokepoint before this method is ever reached;
//! this method owns the rest of the gate the capability check cannot see —
//! the workspace claim ([`OrbitRuntime::require_workspace_claim`]), the
//! working-directory confinement shared with `orbit.agent.invoke`, and the
//! audit record naming what actually ran.

use std::process::Command;
use std::time::Instant;

use orbit_common::OrbitError;
use orbit_common::security::redaction::{PatternRedactor, is_sensitive_env_name};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::ExecutionResult;
use serde_json::json;

use super::coordination_audit::{CoordinationAuditEvent, record_coordination_audit_event};
use crate::OrbitRuntime;

const COMMAND_TOOL_NAME: &str = "orbit.command.exec";
const COMMAND_TARGET_TYPE: &str = "command_exec";

/// Parameters for [`OrbitRuntime::execute_remote_command`], already parsed and
/// typed by the tool-dispatch layer.
pub(crate) struct RemoteCommandParams {
    pub(crate) argv: Vec<String>,
    pub(crate) working_directory: String,
    pub(crate) claim_token: Option<String>,
    pub(crate) actor: String,
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

        let started = Instant::now();
        let result = Command::new(&program)
            .args(&args)
            .current_dir(&working_directory)
            .env_clear()
            .envs(env_pairs)
            .output()
            .map(|output| ExecutionResult {
                success: output.status.success(),
                timed_out: false,
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                exit_code: output.status.code(),
                duration_ms: started.elapsed().as_millis() as u64,
                output: None,
            })
            .map_err(|error| {
                OrbitError::Execution(format!(
                    "spawn '{program}' in '{}': {error}",
                    working_directory.display()
                ))
            });

        let status = if result.is_ok() {
            AuditEventStatus::Success
        } else {
            AuditEventStatus::Failure
        };
        let redaction = PatternRedactor::with_argv_secrets();
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
