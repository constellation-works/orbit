use std::collections::HashMap;

use orbit_common::OrbitError;
use orbit_types::telemetry::InvocationTrace;

use crate::agent::{AgentConfig, ProviderOptions};
use crate::providers::copilot::copilot_cli::CopilotCliTransport;
use crate::runtime::{AgentRuntime, AgentRuntimeFactory};
use crate::types::{AgentInvocationSpec, AgentRequest};

const RUNTIME_KEY: &str = "copilot";

/// Non-credential process context the Copilot CLI needs before it can read
/// Orbit's envelope.
///
/// `COPILOT_HOME` is listed alongside `HOME`/`PATH` because the sandbox grants
/// exactly the directory it names: if the operator sets it but the child never
/// receives it, the CLI would write state to `$HOME/.copilot` while the
/// profile granted the override path, and startup would fail on a denial that
/// points at the wrong directory.
///
/// Copilot's credentials are deliberately **absent** here.
/// `COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, and `GITHUB_TOKEN` are credentials, and
/// `child_env` admits credentials only by operator-named opt-in through
/// `[execution.env].pass`. Orbit never forwards them on the provider's behalf,
/// so a Copilot run cannot silently pick up a GitHub token that an unrelated
/// tool left in the environment. With no token passed, the CLI uses the
/// credentials `copilot /login` stored under `COPILOT_HOME`. [ORB-10946]
/// `XDG_CACHE_HOME` is likewise forwarded: the Copilot launcher extracts its
/// bundled package to `$XDG_CACHE_HOME/copilot`, which the sandbox grants
/// from the same override.
const REQUIRED_ENV_VARS: &[&str] = &["HOME", "PATH", "COPILOT_HOME", "XDG_CACHE_HOME"];

pub(crate) struct CopilotRuntime {
    command: String,
    cli: CopilotCliTransport,
    runtime_key: &'static str,
    required_env_vars: &'static [&'static str],
}

pub(crate) struct CopilotFactory;

impl CopilotRuntime {
    pub(crate) fn new(
        command: String,
        model: Option<String>,
        runtime_key: &'static str,
        required_env_vars: &'static [&'static str],
    ) -> Self {
        Self {
            command,
            cli: CopilotCliTransport::new(model),
            runtime_key,
            required_env_vars,
        }
    }
}

impl AgentRuntimeFactory for CopilotFactory {
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
        Ok(ProviderOptions::Copilot)
    }

    fn build(&self, cfg: &AgentConfig) -> Result<Box<dyn AgentRuntime>, OrbitError> {
        match &cfg.provider_options {
            ProviderOptions::Copilot => Ok(Box::new(CopilotRuntime::new(
                cfg.command.clone(),
                cfg.model.clone(),
                self.key(),
                self.required_env_vars(),
            ))),
            _ => Err(OrbitError::InvalidInput(format!(
                "provider options '{}' cannot build copilot runtime",
                cfg.provider_key
            ))),
        }
    }
}

impl AgentRuntime for CopilotRuntime {
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
