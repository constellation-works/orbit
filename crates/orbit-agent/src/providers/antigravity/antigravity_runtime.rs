use std::collections::HashMap;

use orbit_common::OrbitError;
use orbit_types::identity::validate_antigravity_model;
use orbit_types::telemetry::InvocationTrace;

use crate::agent::{AgentConfig, ProviderOptions};
use crate::providers::antigravity::antigravity_cli::AntigravityCliTransport;
use crate::runtime::{AgentRuntime, AgentRuntimeFactory};
use crate::types::{AgentInvocationSpec, AgentRequest};

const RUNTIME_KEY: &str = "agy";

/// Non-secret process context required by the local Antigravity CLI.
///
/// Credentials stay in `~/.gemini/antigravity-cli/` from an interactive `agy`
/// login. Orbit never auto-logs in and never rewrites those files. [ORB-11299]
const REQUIRED_ENV_VARS: &[&str] = &["HOME", "PATH"];

pub(crate) struct AntigravityRuntime {
    command: String,
    cli: AntigravityCliTransport,
    runtime_key: &'static str,
    required_env_vars: &'static [&'static str],
}

pub(crate) struct AntigravityFactory;

impl AntigravityRuntime {
    pub(crate) fn new(
        command: String,
        model: Option<String>,
        reasoning_effort: Option<orbit_types::identity::ReasoningEffort>,
        runtime_key: &'static str,
        required_env_vars: &'static [&'static str],
    ) -> Self {
        Self {
            command,
            cli: AntigravityCliTransport::new(model, reasoning_effort),
            runtime_key,
            required_env_vars,
        }
    }
}

impl AgentRuntimeFactory for AntigravityFactory {
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
        Ok(ProviderOptions::Antigravity)
    }

    fn build(&self, cfg: &AgentConfig) -> Result<Box<dyn AgentRuntime>, OrbitError> {
        match &cfg.provider_options {
            ProviderOptions::Antigravity => {
                validate_antigravity_model(cfg.model.as_deref())
                    .map_err(OrbitError::InvalidInput)?;
                Ok(Box::new(AntigravityRuntime::new(
                    cfg.command.clone(),
                    cfg.model.clone(),
                    cfg.reasoning_effort,
                    self.key(),
                    self.required_env_vars(),
                )))
            }
            _ => Err(OrbitError::InvalidInput(format!(
                "provider options '{}' cannot build antigravity runtime",
                cfg.provider_key
            ))),
        }
    }
}

impl AgentRuntime for AntigravityRuntime {
    fn invoke(
        &self,
        req: AgentRequest,
    ) -> Result<(AgentInvocationSpec, InvocationTrace), OrbitError> {
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
