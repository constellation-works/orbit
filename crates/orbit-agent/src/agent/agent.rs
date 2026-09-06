use std::collections::HashMap;

use orbit_common::OrbitError;
use orbit_types::identity::ReasoningEffort;
use orbit_types::telemetry::InvocationTrace;

use crate::runtime::{AgentRuntime, ProviderRegistry, resolve_runtime};
use crate::types::{AgentInvocationSpec, AgentRequest};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderOptions {
    Claude,
    Codex {
        sandbox: String,
        approval_policy: Option<String>,
        writable_dirs: Vec<String>,
    },
    Gemini,
    Antigravity,
    Grok,
    Copilot,
    Cursor,
    Ollama,
    Pi,
    Opencode,
    Mock,
}

impl ProviderOptions {
    /// The canonical provider identity used for cross-provider validation
    /// (e.g. [`ReasoningEffort::validate_for_provider_model`]), independent
    /// of `AgentConfig::provider_key` — the registry dispatch key, which is
    /// derived from the CLI executable name and can diverge from it (the
    /// Antigravity executable is `agy`, but its canonical identity is
    /// `antigravity`).
    fn canonical_provider_name(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex { .. } => "codex",
            Self::Gemini => "gemini",
            Self::Antigravity => "antigravity",
            Self::Grok => "grok",
            Self::Copilot => "copilot",
            Self::Cursor => "cursor",
            Self::Ollama => "ollama",
            Self::Pi => "pi",
            Self::Opencode => "opencode",
            Self::Mock => "mock",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentConfig {
    pub command: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub provider_key: &'static str,
    pub provider_options: ProviderOptions,
}

impl AgentConfig {
    /// Construct an `AgentConfig` from a CLI binary name, detecting the
    /// provider automatically.  Codex defaults to `workspace-write` sandbox
    /// and no approval-policy override; use `AgentConfig::from_cli_config`
    /// directly when non-default provider settings are required.
    pub fn cli(command: impl Into<String>) -> Result<Self, OrbitError> {
        Self::from_cli_config(command, None, &HashMap::new())
    }

    pub fn from_cli_config(
        command: impl Into<String>,
        model: Option<&str>,
        config: &HashMap<String, String>,
    ) -> Result<Self, OrbitError> {
        let command = command.into();
        let registry = ProviderRegistry::default();
        let factory = registry.factory_for_cli(&command)?;
        Ok(Self {
            command,
            model: model.map(ToString::to_string),
            reasoning_effort: None,
            provider_key: factory.key(),
            provider_options: factory.options_from_config(config)?,
        })
    }

    pub fn with_model(mut self, model: Option<&str>) -> Self {
        self.model = model.map(ToString::to_string);
        self
    }

    /// Attach effort resolved from a crew after verifying the provider-model
    /// CLI contract that will receive it.
    pub fn with_reasoning_effort(
        mut self,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> Result<Self, OrbitError> {
        if let Some(effort) = reasoning_effort {
            effort
                .validate_for_provider_model(
                    self.provider_options.canonical_provider_name(),
                    self.model.as_deref(),
                )
                .map_err(OrbitError::InvalidInput)?;
        }
        self.reasoning_effort = reasoning_effort;
        Ok(self)
    }
}

pub struct Agent {
    runtime: Box<dyn AgentRuntime>,
}

impl Agent {
    pub fn new(cfg: &AgentConfig) -> Result<Self, OrbitError> {
        let registry = ProviderRegistry::default();
        Ok(Self {
            runtime: resolve_runtime(&registry, cfg)?,
        })
    }

    pub fn invoke(
        &self,
        req: AgentRequest,
    ) -> Result<(AgentInvocationSpec, InvocationTrace), OrbitError> {
        self.runtime.invoke(req)
    }

    pub fn model_name(&self) -> Option<&str> {
        self.runtime.model_name()
    }
}
