use std::collections::HashMap;

use crate::agent::{AgentConfig, ProviderOptions};
use crate::providers::codex::codex_cli::CodexCliTransport;
use crate::runtime::{AgentRuntime, AgentRuntimeFactory};
use crate::types::{AgentInvocationSpec, AgentRequest};
use orbit_common::OrbitError;
use orbit_types::telemetry::InvocationTrace;

const RUNTIME_KEY: &str = "codex";
const REQUIRED_ENV_VARS: &[&str] = &["HOME", "PATH"];

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
        Ok(ProviderOptions::Codex {
            sandbox,
            approval_policy,
            writable_dirs,
        })
    }

    fn build(&self, cfg: &AgentConfig) -> Result<Box<dyn AgentRuntime>, OrbitError> {
        match &cfg.provider_options {
            ProviderOptions::Codex {
                sandbox,
                approval_policy,
                writable_dirs,
            } => Ok(Box::new(CodexRuntime::new(
                cfg.command.clone(),
                CodexCliTransport::new(
                    cfg.model.clone(),
                    cfg.reasoning_effort,
                    sandbox.clone(),
                    approval_policy.clone(),
                    writable_dirs.clone(),
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
                self.cli.args(),
                self.cli.stdin(&req.envelope_json),
            ),
            InvocationTrace::default(),
        ))
    }

    fn model_name(&self) -> Option<&str> {
        self.cli.model_name()
    }
}
