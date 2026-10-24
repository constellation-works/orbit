use orbit_common::OrbitError;
use orbit_types::identity::{AgentModelPair, normalize_agent_family_for_model};

use crate::RuntimeKernel;

pub(super) fn normalize_agent_name(agent_cli: &str) -> String {
    std::path::Path::new(agent_cli)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(agent_cli)
        .to_ascii_lowercase()
}

impl RuntimeKernel {
    pub fn configured_agent_model_pair(&self, agent_cli: &str) -> Option<AgentModelPair> {
        self.stores()
            .executors()
            .get_executor_def(agent_cli)
            .ok()
            .flatten()
            .and_then(|def| {
                def.model_pair_override()
                    .map(|pair| AgentModelPair::new(pair.strong.clone(), pair.weak.clone()))
            })
    }

    pub(crate) fn canonical_agent_model_identity(
        &self,
        agent_cli: Option<&str>,
        model: Option<&str>,
    ) -> (Option<String>, Option<String>) {
        self.try_canonical_agent_model_identity(agent_cli, model)
            .unwrap_or_else(|_| self.legacy_canonical_agent_model_identity(agent_cli, model))
    }

    pub fn invocation_agent_model_identity(
        &self,
        agent_cli: &str,
        requested_model: Option<&str>,
        provider_model: Option<&str>,
        job_run_id: &str,
        activity_id: &str,
    ) -> (Option<String>, Option<String>) {
        let (agent, requested_model) =
            self.canonical_agent_model_identity(Some(agent_cli), requested_model);
        let provider_model = provider_model
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);

        let stored_model = match (requested_model.as_deref(), provider_model.as_deref()) {
            (Some(requested), Some(provider))
                if grok_build_ledger_aliases_requested(requested, provider) =>
            {
                Some(requested.to_string())
            }
            (Some(requested), Some(provider)) if requested != provider => {
                tracing::warn!(
                    target: "orbit.core.invocation",
                    job_run_id,
                    activity_id,
                    agent_cli,
                    requested_model = requested,
                    provider_model = provider,
                    "provider-reported model differs from requested model",
                );
                Some(provider.to_string())
            }
            (_, Some(provider)) => Some(provider.to_string()),
            (_, None) => requested_model,
        };

        (agent, stored_model)
    }

    pub fn try_canonical_agent_model_identity(
        &self,
        agent_cli: Option<&str>,
        model: Option<&str>,
    ) -> Result<(Option<String>, Option<String>), OrbitError> {
        let agent = normalize_agent_family_for_model(agent_cli, model)?;
        let model = model
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        Ok((agent, model))
    }

    fn legacy_canonical_agent_model_identity(
        &self,
        agent_cli: Option<&str>,
        model: Option<&str>,
    ) -> (Option<String>, Option<String>) {
        let agent = agent_cli
            .map(normalize_agent_name)
            .filter(|value| !value.trim().is_empty());
        let model = model
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        (agent, model)
    }
}

/// Grok Build's usage ledger names requested public menu id `grok-X.Y` as
/// `grok-X.Y-build`. That suffix is a billing label, not a different public
/// model, so ingest stores the requested id used by the shipped price table.
fn grok_build_ledger_aliases_requested(requested: &str, provider: &str) -> bool {
    requested.starts_with("grok-")
        && provider
            .strip_suffix("-build")
            .is_some_and(|public_id| public_id == requested)
}
