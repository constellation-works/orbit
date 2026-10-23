use std::collections::HashMap;

use orbit_common::OrbitError;
use orbit_types::identity::ReasoningEffort;
use orbit_types::telemetry::InvocationTrace;

use crate::agent::{AgentConfig, ProviderOptions};
use crate::providers::muse::muse_cli::{MUSE_PROMPT_FILE_FLAG, MuseCliTransport};
use crate::runtime::{AgentRuntime, AgentRuntimeFactory};
use crate::types::{AgentInvocationSpec, AgentRequest};

const RUNTIME_KEY: &str = "muse";

/// Non-secret process context required by the local Muse Code CLI.
///
/// No `META_API_KEY` appears here. Muse resolves credentials from its
/// `muse login` state (`auth.json` under its config directory). An operator
/// who deliberately uses an API key names `META_API_KEY` in
/// `[execution.env].pass`; Orbit never renders a credential onto argv. The
/// XDG variables locate Muse state and are safe to pass through as runtime
/// requirements.
/// [ORB-16577]
const REQUIRED_ENV_VARS: &[&str] = &["HOME", "PATH", "XDG_CONFIG_HOME", "XDG_DATA_HOME"];

/// Launcher switches pinned for every run. The `muse` launcher otherwise
/// checks for updates hourly and swaps its binary in the background, and can
/// start a device login when stderr is a TTY. A managed run must never
/// replace the binary under running work or begin a login; upgrades and
/// sign-in stay with the operator. [ORB-16577]
const MUSE_CLI_FIXED_ENV: &[(&str, &str)] = &[("MUSE_NO_AUTO_UPDATE", "1"), ("MUSE_LOGIN", "0")];

pub(crate) struct MuseRuntime {
    command: String,
    cli: MuseCliTransport,
    runtime_key: &'static str,
    required_env_vars: &'static [&'static str],
}

pub(crate) struct MuseFactory;

impl MuseRuntime {
    pub(crate) fn new(
        command: String,
        model: Option<String>,
        reasoning_effort: Option<ReasoningEffort>,
        runtime_key: &'static str,
        required_env_vars: &'static [&'static str],
    ) -> Self {
        Self {
            command,
            cli: MuseCliTransport::new(model, reasoning_effort),
            runtime_key,
            required_env_vars,
        }
    }
}

impl AgentRuntimeFactory for MuseFactory {
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
        Ok(ProviderOptions::Muse)
    }

    fn build(&self, cfg: &AgentConfig) -> Result<Box<dyn AgentRuntime>, OrbitError> {
        match &cfg.provider_options {
            ProviderOptions::Muse => Ok(Box::new(MuseRuntime::new(
                cfg.command.clone(),
                cfg.model.clone(),
                cfg.reasoning_effort,
                self.key(),
                self.required_env_vars(),
            ))),
            _ => Err(OrbitError::InvalidInput(format!(
                "provider options '{}' cannot build muse runtime",
                cfg.provider_key
            ))),
        }
    }
}

impl AgentRuntime for MuseRuntime {
    fn invoke(
        &self,
        req: AgentRequest,
    ) -> Result<(AgentInvocationSpec, InvocationTrace), OrbitError> {
        let mut spec = crate::providers::build_invocation_spec(
            self.runtime_key,
            self.required_env_vars,
            self.command.clone(),
            self.cli.args(),
            self.cli.prompt(&req.envelope_json),
        );
        spec.fixed_env = MUSE_CLI_FIXED_ENV;
        spec.prompt_file_flag = Some(MUSE_PROMPT_FILE_FLAG);
        // The retained `run.terminal.completed` record carries no usage in
        // Muse Code 1.4.10. Account windows (1.4.12) come from the separate
        // `status` / `run_usage` allowance via `provider_usage_windows`; per-run
        // token accounting remains unreported here.
        Ok((spec, InvocationTrace::default()))
    }

    fn model_name(&self) -> Option<&str> {
        self.cli.model_name()
    }
}
