use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use orbit_common::OrbitError;

use crate::runtime::AgentRuntimeFactory;

pub(crate) struct ProviderRegistry {
    factories: HashMap<&'static str, Arc<dyn AgentRuntimeFactory>>,
}

impl ProviderRegistry {
    pub(crate) fn new() -> Self {
        Self {
            factories: HashMap::new(),
        }
    }

    pub(crate) fn register(
        &mut self,
        factory: Arc<dyn AgentRuntimeFactory>,
    ) -> Option<Arc<dyn AgentRuntimeFactory>> {
        self.factories.insert(factory.key(), factory)
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Arc<dyn AgentRuntimeFactory>> {
        self.factories.get(key)
    }

    pub(crate) fn factory_for_cli(
        &self,
        agent_cli: &str,
    ) -> Result<&Arc<dyn AgentRuntimeFactory>, OrbitError> {
        let key = Self::normalize_cli_key(agent_cli);
        self.factories
            .get(key.as_str())
            .ok_or(OrbitError::UnsupportedAgentProvider(key))
    }

    fn normalize_cli_key(agent_cli: &str) -> String {
        Path::new(agent_cli)
            .file_name()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .unwrap_or_else(|| agent_cli.to_ascii_lowercase())
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        let mut registry = Self::new();
        let _ = registry.register(Arc::new(crate::providers::mock_agent::MockAgentFactory));
        let _ = registry.register(Arc::new(crate::providers::codex::CodexFactory));
        let _ = registry.register(Arc::new(crate::providers::claude::ClaudeFactory));
        let _ = registry.register(Arc::new(crate::providers::gemini::GeminiFactory));
        let _ = registry.register(Arc::new(crate::providers::antigravity::AntigravityFactory));
        let _ = registry.register(Arc::new(crate::providers::grok::GrokFactory));
        let _ = registry.register(Arc::new(crate::providers::copilot::CopilotFactory));
        let _ = registry.register(Arc::new(crate::providers::cursor::CursorFactory));
        let _ = registry.register(Arc::new(crate::providers::ollama::OllamaFactory));
        let _ = registry.register(Arc::new(crate::providers::pi::PiFactory));
        registry
    }
}
