use std::collections::HashMap;

use orbit_common::OrbitError;
use orbit_types::identity::ReasoningEffort;
use orbit_types::telemetry::InvocationTrace;

use crate::agent::{AgentConfig, ProviderOptions};
use crate::providers::opencode::opencode_cli::OpencodeCliTransport;
use crate::runtime::{AgentRuntime, AgentRuntimeFactory};
use crate::types::{AgentInvocationSpec, AgentRequest};

const RUNTIME_KEY: &str = "opencode";

/// Non-secret process context required by the local OpenCode CLI.
///
/// No `*_API_KEY` appears here. OpenCode resolves credentials from
/// `opencode auth login` state under its XDG data directory
/// (`$XDG_DATA_HOME/opencode`, default `$HOME/.local/share/opencode`).
/// An operator who deliberately uses an API key instead names its variable in
/// `[execution.env].pass`; Orbit never renders a credential onto argv.
/// [ORB-11295]
const REQUIRED_ENV_VARS: &[&str] = &["HOME", "PATH"];

pub(crate) struct OpencodeRuntime {
    command: String,
    cli: OpencodeCliTransport,
    runtime_key: &'static str,
    required_env_vars: &'static [&'static str],
}

pub(crate) struct OpencodeFactory;

impl OpencodeRuntime {
    pub(crate) fn new(
        command: String,
        model: Option<String>,
        reasoning_effort: Option<ReasoningEffort>,
        runtime_key: &'static str,
        required_env_vars: &'static [&'static str],
    ) -> Self {
        Self {
            command,
            cli: OpencodeCliTransport::new(model, reasoning_effort),
            runtime_key,
            required_env_vars,
        }
    }
}

impl AgentRuntimeFactory for OpencodeFactory {
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
        Ok(ProviderOptions::Opencode)
    }

    fn build(&self, cfg: &AgentConfig) -> Result<Box<dyn AgentRuntime>, OrbitError> {
        match &cfg.provider_options {
            ProviderOptions::Opencode => Ok(Box::new(OpencodeRuntime::new(
                cfg.command.clone(),
                cfg.model.clone(),
                cfg.reasoning_effort,
                self.key(),
                self.required_env_vars(),
            ))),
            _ => Err(OrbitError::InvalidInput(format!(
                "provider options '{}' cannot build opencode runtime",
                cfg.provider_key
            ))),
        }
    }
}

impl AgentRuntime for OpencodeRuntime {
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
            // OpenCode reports token usage on `step_finish` parts, which
            // normalization drops before any Orbit read: those frames are the
            // provider's own accounting, not the model-authored answer, and
            // keeping them would put tool traffic back in front of the envelope
            // reader. Orbit therefore claims no usage here rather than
            // inventing one; the invocation trace stays whatever the Orbit
            // response envelope itself declares.
            InvocationTrace::default(),
        ))
    }

    fn model_name(&self) -> Option<&str> {
        self.cli.model_name()
    }
}
