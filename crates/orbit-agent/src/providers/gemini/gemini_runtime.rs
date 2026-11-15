use std::collections::HashMap;

use orbit_common::OrbitError;

use crate::agent::{AgentConfig, ProviderOptions};
use crate::providers::gemini::gemini_cli::GeminiCliTransport;
use crate::runtime::{AgentRuntime, AgentRuntimeFactory};
use crate::types::{AgentInvocationSpec, AgentRequest};

const RUNTIME_KEY: &str = "gemini";
const REQUIRED_ENV_VARS: &[&str] = &["HOME", "PATH"];

pub(crate) struct GeminiRuntime {
    command: String,
    cli: GeminiCliTransport,
    required_env_vars: &'static [&'static str],
}

pub(crate) struct GeminiFactory;

impl GeminiRuntime {
    pub(crate) fn new(
        command: String,
        model: Option<String>,
        required_env_vars: &'static [&'static str],
    ) -> Self {
        Self {
            command,
            cli: GeminiCliTransport::new(model),
            required_env_vars,
        }
    }
}

impl AgentRuntimeFactory for GeminiFactory {
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
        Ok(ProviderOptions::Gemini)
    }

    fn build(&self, cfg: &AgentConfig) -> Result<Box<dyn AgentRuntime>, OrbitError> {
        match &cfg.provider_options {
            ProviderOptions::Gemini => Ok(Box::new(GeminiRuntime::new(
                cfg.command.clone(),
                cfg.model.clone(),
                self.required_env_vars(),
            ))),
            _ => Err(OrbitError::InvalidInput(format!(
                "provider options '{}' cannot build gemini runtime",
                cfg.provider_key
            ))),
        }
    }
}

impl AgentRuntime for GeminiRuntime {
    fn invoke(&self, req: AgentRequest) -> Result<AgentInvocationSpec, OrbitError> {
        Ok(crate::providers::build_invocation_spec(
            self.required_env_vars,
            self.command.clone(),
            self.cli.args(),
            self.cli.stdin(&req.envelope_json),
        ))
    }

    fn model_name(&self) -> Option<&str> {
        self.cli.model_name()
    }
}
