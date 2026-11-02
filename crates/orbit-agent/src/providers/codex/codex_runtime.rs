use std::collections::HashMap;

use crate::agent::{AgentConfig, ProviderOptions};
use crate::providers::codex::codex_cli::CodexCliTransport;
use crate::runtime::{AgentRuntime, AgentRuntimeFactory};
use crate::types::{AgentInvocationSpec, AgentRequest};
use orbit_common::OrbitError;
use orbit_types::telemetry::InvocationTrace;

const RUNTIME_KEY: &str = "codex";
/// `CODEX_HOME` is non-secret state-root context: the sandbox grants exactly
/// the directory it names, so dropping it would make the CLI resolve
/// `$HOME/.codex` while the profile granted the override.
const REQUIRED_ENV_VARS: &[&str] = &["HOME", "PATH", "CODEX_HOME"];

pub(crate) struct CodexRuntime {
    command: String,
    cli: CodexCliTransport,
    runtime_key: &'static str,
    required_env_vars: &'static [&'static str],
}

pub(crate) struct CodexFactory;

impl CodexRuntime {
    pub(crate) fn new(
        command: String,
        cli: CodexCliTransport,
        runtime_key: &'static str,
        required_env_vars: &'static [&'static str],
    ) -> Self {
        Self {
            command,
            cli,
            runtime_key,
            required_env_vars,
        }
    }
}

impl AgentRuntimeFactory for CodexFactory {
    fn key(&self) -> &'static str {
        RUNTIME_KEY
    }

    fn required_env_vars(&self) -> &'static [&'static str] {
        REQUIRED_ENV_VARS
    }

    fn options_from_config(
        &self,
        config: &HashMap<String, String>,
    ) -> Result<ProviderOptions, OrbitError> {
        let sandbox = config
            .get("sandbox")
            .cloned()
            .unwrap_or_else(|| "workspace-write".to_string());
        let approval_policy = config.get("approval_policy").cloned();
        let writable_dirs = config
            .get("writable_dirs_json")
            .map(|raw| {
                serde_json::from_str::<Vec<String>>(raw).map_err(|err| {
                    OrbitError::InvalidInput(format!(
                        "invalid codex writable_dirs_json provider option: {err}"
                    ))
                })
            })
            .transpose()?
            .unwrap_or_default();
        let allow_login_shell = config
            .get("allow_login_shell")
            .map(|raw| match raw.as_str() {
                "true" => Ok(true),
                "false" => Ok(false),
                other => Err(OrbitError::InvalidInput(format!(
                    "invalid codex allow_login_shell provider option `{other}`: use true or false"
                ))),
            })
            .transpose()?;
        Ok(ProviderOptions::Codex {
            sandbox,
            approval_policy,
            writable_dirs,
            allow_login_shell,
        })
    }

    fn build(&self, cfg: &AgentConfig) -> Result<Box<dyn AgentRuntime>, OrbitError> {
        match &cfg.provider_options {
            ProviderOptions::Codex {
                sandbox,
                approval_policy,
                writable_dirs,
                allow_login_shell,
            } => Ok(Box::new(CodexRuntime::new(
                cfg.command.clone(),
                CodexCliTransport::new(
                    cfg.model.clone(),
                    cfg.reasoning_effort,
                    sandbox.clone(),
                    approval_policy.clone(),
                    writable_dirs.clone(),
                    *allow_login_shell,
                ),
                self.key(),
                self.required_env_vars(),
            ))),
            _ => Err(OrbitError::InvalidInput(format!(
                "provider options '{}' cannot build codex runtime",
                cfg.provider_key
            ))),
        }
    }
}

impl AgentRuntime for CodexRuntime {
    fn invoke(
        &self,
        req: AgentRequest,
    ) -> Result<(AgentInvocationSpec, InvocationTrace), OrbitError> {
        // Note: stdout_schema_json is intentionally None — Codex rejects Orbit's
        // generic envelope schema because open-ended object branches must be closed
        // with additionalProperties=false. Orbit validates the returned envelope
        // after execution instead.
        Ok((
            crate::providers::build_invocation_spec(
                self.runtime_key,
                self.required_env_vars,
                self.command.clone(),
                self.cli.args(req.conversation_ref.as_deref()),
                self.cli.stdin(&req.envelope_json),
            ),
            InvocationTrace::default(),
        ))
    }

    fn model_name(&self) -> Option<&str> {
        self.cli.model_name()
    }

    fn supports_continuity(&self) -> bool {
        true
    }

    /// A resume failed only when Codex could not use the stored session: the
    /// run did not complete, and its diagnostics name a missing, unknown or
    /// expired session. A `type:error` frame alone is a transient notice (for
    /// example `Reconnecting...`) that a turn survives, and a failure for any
    /// other reason (auth, network, model) must surface rather than re-run the
    /// brief.
    fn resume_failed(&self, exit_code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> bool {
        let mut turn_failed = false;
        let mut turn_completed = false;
        let mut diagnostics = String::from_utf8_lossy(stderr).into_owned();
        for (_, frame) in crate::providers::usage_window::top_level_frames(stdout) {
            match frame.get("type").and_then(serde_json::Value::as_str) {
                Some("turn.completed") => {
                    turn_completed = true;
                    turn_failed = false;
                }
                Some("turn.failed") => {
                    turn_failed = true;
                    turn_completed = false;
                    push_frame_message(&mut diagnostics, &frame);
                }
                Some("error") => push_frame_message(&mut diagnostics, &frame),
                _ => {}
            }
        }
        let run_succeeded = exit_code == Some(0) && !turn_failed || turn_completed;
        !run_succeeded && names_unusable_session(&diagnostics)
    }
}

fn push_frame_message(diagnostics: &mut String, frame: &serde_json::Value) {
    for pointer in ["/message", "/error/message"] {
        if let Some(message) = frame.pointer(pointer).and_then(serde_json::Value::as_str) {
            diagnostics.push('\n');
            diagnostics.push_str(message);
        }
    }
}

/// Whether diagnostics say the resumed session is missing, unknown or expired.
fn names_unusable_session(diagnostics: &str) -> bool {
    const MARKERS: &[&str] = &[
        "no saved session",
        "no rollout found",
        "session not found",
        "conversation not found",
        "thread not found",
        "unknown session",
        "invalid session",
    ];
    const AUTH_WORDS: &[&str] = &["token", "auth", "login", "log in", "credential", "api key"];
    let lower = diagnostics.to_ascii_lowercase();
    if MARKERS.iter().any(|marker| lower.contains(marker)) {
        return true;
    }
    // An expired session, unless it is the account login that expired.
    lower.contains("session")
        && lower.contains("expired")
        && !AUTH_WORDS.iter().any(|word| lower.contains(word))
}
