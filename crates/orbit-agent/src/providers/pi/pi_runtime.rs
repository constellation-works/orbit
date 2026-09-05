use std::collections::HashMap;

use orbit_common::OrbitError;
use orbit_types::identity::ReasoningEffort;
use orbit_types::telemetry::InvocationTrace;

use crate::agent::{AgentConfig, ProviderOptions};
use crate::providers::pi::pi_cli::PiCliTransport;
use crate::runtime::{AgentRuntime, AgentRuntimeFactory};
use crate::types::{AgentInvocationSpec, AgentRequest};

const RUNTIME_KEY: &str = "pi";

/// Non-secret process context required by the local Pi CLI.
///
/// No `*_API_KEY` appears here, and Orbit never renders Pi's `--api-key` flag.
/// Credentials cross the cleared child environment only when an operator names
/// them in `[execution.env].pass`; otherwise Pi uses the login state under its
/// own agent directory (`$PI_CODING_AGENT_DIR`, default `$HOME/.pi/agent`).
/// [ORB-11296]
const REQUIRED_ENV_VARS: &[&str] = &["HOME", "PATH"];

pub(crate) struct PiRuntime {
    command: String,
    cli: PiCliTransport,
    runtime_key: &'static str,
    required_env_vars: &'static [&'static str],
}

pub(crate) struct PiFactory;

impl PiRuntime {
    pub(crate) fn new(
        command: String,
        model: Option<String>,
        reasoning_effort: Option<ReasoningEffort>,
        runtime_key: &'static str,
        required_env_vars: &'static [&'static str],
    ) -> Self {
        Self {
            command,
            cli: PiCliTransport::new(model, reasoning_effort),
            runtime_key,
            required_env_vars,
        }
    }
}

impl AgentRuntimeFactory for PiFactory {
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
        Ok(ProviderOptions::Pi)
    }

    fn build(&self, cfg: &AgentConfig) -> Result<Box<dyn AgentRuntime>, OrbitError> {
        match &cfg.provider_options {
            ProviderOptions::Pi => Ok(Box::new(PiRuntime::new(
                cfg.command.clone(),
                cfg.model.clone(),
                cfg.reasoning_effort,
                self.key(),
                self.required_env_vars(),
            ))),
            _ => Err(OrbitError::InvalidInput(format!(
                "provider options '{}' cannot build pi runtime",
                cfg.provider_key
            ))),
        }
    }
}

impl AgentRuntime for PiRuntime {
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
            // Pi reports cumulative provider usage on its streaming control
            // plane, which normalization drops before any Orbit read. Orbit
            // therefore claims no usage here rather than inventing one; the
            // invocation trace stays whatever the Orbit response envelope
            // itself declares.
            InvocationTrace::default(),
        ))
    }

    fn model_name(&self) -> Option<&str> {
        self.cli.model_name()
    }
}
