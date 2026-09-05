use std::collections::HashMap;

use orbit_common::OrbitError;
use orbit_types::telemetry::InvocationTrace;

use crate::agent::{AgentConfig, ProviderOptions};
use crate::providers::claude::claude_cli::ClaudeCliTransport;
use crate::runtime::{AgentRuntime, AgentRuntimeFactory};
use crate::types::{AgentInvocationSpec, AgentRequest};

const RUNTIME_KEY: &str = "claude";
const REQUIRED_ENV_VARS: &[&str] = &["HOME", "PATH"];

pub(crate) struct ClaudeRuntime {
    command: String,
    cli: ClaudeCliTransport,
    runtime_key: &'static str,
    required_env_vars: &'static [&'static str],
}

pub(crate) struct ClaudeFactory;

impl ClaudeRuntime {
    pub(crate) fn new(
        command: String,
        model: Option<String>,
        reasoning_effort: Option<orbit_types::identity::ReasoningEffort>,
        runtime_key: &'static str,
        required_env_vars: &'static [&'static str],
    ) -> Self {
        Self {
            command,
            cli: ClaudeCliTransport::new(model, reasoning_effort),
            runtime_key,
            required_env_vars,
        }
    }
}

impl AgentRuntimeFactory for ClaudeFactory {
    fn key(&self) -> &'static str {
        RUNTIME_KEY
    }

    fn required_env_vars(&self) -> &'static [&'static str] {
        REQUIRED_ENV_VARS
    }

    fn options_from_config(
        &self,
        _config: &HashMap<String, String>,
    ) -> Result<ProviderOptions, OrbitError> {
        Ok(ProviderOptions::Claude)
    }

    fn build(&self, cfg: &AgentConfig) -> Result<Box<dyn AgentRuntime>, OrbitError> {
        match &cfg.provider_options {
            ProviderOptions::Claude => Ok(Box::new(ClaudeRuntime::new(
                cfg.command.clone(),
                cfg.model.clone(),
                cfg.reasoning_effort,
                self.key(),
                self.required_env_vars(),
            ))),
            _ => Err(OrbitError::InvalidInput(format!(
                "provider options '{}' cannot build claude runtime",
                cfg.provider_key
            ))),
        }
    }
}

impl AgentRuntime for ClaudeRuntime {
    fn invoke(
        &self,
        req: AgentRequest,
    ) -> Result<(AgentInvocationSpec, InvocationTrace), OrbitError> {
        Ok((
            crate::providers::build_invocation_spec(
                self.runtime_key,
                self.required_env_vars,
                self.command.clone(),
                self.cli.args(req.verbose),
                self.cli.stdin(&req.envelope_json),
            ),
            InvocationTrace::default(),
        ))
    }

    fn model_name(&self) -> Option<&str> {
        self.cli.model_name()
    }
}
