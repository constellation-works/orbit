use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::optional_string_alias;
use orbit_types::identity::{normalize_agent_family_for_model, require_canonical_agent_family};
use orbit_types::tool::ToolParam;
use serde_json::Value;

use crate::ToolContext;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct OrbitIdentity {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub actor_label: Option<String>,
}

fn trimmed_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn resolve_identity(
    ctx: &ToolContext,
    input: &Value,
) -> Result<OrbitIdentity, OrbitError> {
    if let Some(actor_label) = trimmed_optional(ctx.trusted_actor_label.clone()) {
        return Ok(OrbitIdentity {
            agent: None,
            model: Some(actor_label.clone()),
            actor_label: Some(actor_label),
        });
    }

    // `model` is no longer a schema parameter, so the registry's null-as-absent
    // rule for declared optional fields does not cover it. A JSON null means no
    // self-reported model, the same as an absent key.
    let input_model = match input.get("model") {
        None | Some(Value::Null) => None,
        Some(_) => optional_string_alias(input, &["model"])?,
    };
    let context_agent = trimmed_optional(ctx.agent_name.clone());
    let context_model = trimmed_optional(ctx.model_name.clone());
    let context_has_identity = context_agent.is_some() || context_model.is_some();
    let input_has_identity = input_model
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    let (agent, model) = if context_has_identity {
        let agent =
            normalize_agent_family_for_model(context_agent.as_deref(), context_model.as_deref())?;
        // Runtime identity takes precedence over any self-reported input.
        (agent, context_model)
    } else if input_has_identity {
        (None, trimmed_optional(input_model))
    } else {
        (None, None)
    };
    let family = require_canonical_agent_family(agent.as_deref(), model.as_deref())?;
    let model = model.or_else(|| family.clone());
    Ok(OrbitIdentity {
        agent: family.clone(),
        model,
        actor_label: family,
    })
}

pub(super) fn identity_params() -> Vec<ToolParam> {
    Vec::new()
}

pub(super) fn model_identity_params() -> Vec<ToolParam> {
    Vec::new()
}
