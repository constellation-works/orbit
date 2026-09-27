use crate::providers::common::render_prompt_with_embedded_envelope;
use orbit_common::security::child_env::MCP_MANAGED_BINDING_ENV_VARS;
use orbit_types::identity::ReasoningEffort;

fn codex_config_string_arg(key: &str, value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("{key}=\"{escaped}\"")
}

pub(crate) struct CodexCliTransport {
    model: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
    sandbox: String,
    approval_policy: Option<String>,
    writable_dirs: Vec<String>,
}

impl CodexCliTransport {
    pub(crate) fn new(
        model: Option<String>,
        reasoning_effort: Option<ReasoningEffort>,
        sandbox: String,
        approval_policy: Option<String>,
        writable_dirs: Vec<String>,
    ) -> Self {
        Self {
            model,
            reasoning_effort,
            sandbox,
            approval_policy,
            writable_dirs,
        }
    }

    // The transport supplies a complete Orbit MCP entry so its env_vars
    // override is valid even when the user's Codex config has no entry.
    // Managed dispatch replaces the fallback command with its selected binary.
    pub(crate) fn args(&self) -> Vec<String> {
        let mut args = Vec::new();
        let names = MCP_MANAGED_BINDING_ENV_VARS
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(",");
        args.push("--config".to_string());
        args.push(codex_config_string_arg(
            "mcp_servers.orbit.command",
            "orbit",
        ));
        args.push("--config".to_string());
        args.push("mcp_servers.orbit.args=[\"mcp\",\"serve\"]".to_string());
        args.push("--config".to_string());
        args.push("mcp_servers.orbit.enabled=true".to_string());
        args.push("--config".to_string());
        args.push(format!("mcp_servers.orbit.env_vars=[{names}]"));
        if let Some(approval_policy) = &self.approval_policy {
            args.push("--config".to_string());
            args.push(codex_config_string_arg("approval_policy", approval_policy));
        }
        if let Some(model) = &self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if let Some(effort) = self.reasoning_effort {
            args.push("--config".to_string());
            args.push(codex_config_string_arg(
                "model_reasoning_effort",
                &effort.to_string(),
            ));
        }
        args.push("--sandbox".to_string());
        args.push(self.sandbox.clone());
        for dir in &self.writable_dirs {
            args.push("--add-dir".to_string());
            args.push(dir.clone());
        }
        args
    }

    pub(crate) fn stdin(&self, envelope_json: &[u8]) -> Vec<u8> {
        render_prompt_with_embedded_envelope(envelope_json)
    }

    pub(crate) fn model_name(&self) -> Option<&str> {
        self.model.as_deref()
    }
}
