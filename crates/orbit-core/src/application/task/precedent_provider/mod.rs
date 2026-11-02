//! Default-off precedent retrieval. Sources have one whole-response validator;
//! the result preserves gaps, omissions and reversed contradiction evidence.
//! This module retrieves evidence only and never answers a decision request.

mod history;
mod model;
mod validation;

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_types::plugin::{PluginExecutionKind, plugin_tool_name};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::{McpCapability, ToolSessionContext};

use crate::OrbitRuntime;
use crate::runtime::audit::system::{self, AuditRunAttribution, SystemAuditEvent};

pub use history::{decision_precedent_category, decision_precedents_from_history};
pub use model::*;
pub use validation::{validate_precedent_request, validate_precedent_response};

impl OrbitRuntime {
    /// Retrieve only under an exact host workspace grant. Missing grants return
    /// disabled evidence without scanning histories or calling plugins. A bad
    /// source contributes a gap and no rows; other sources remain available.
    /// Consumers persist this entire result with their immutable packet.
    pub fn retrieve_decision_precedents(
        &self,
        request: &PrecedentProviderRequest,
    ) -> Result<PrecedentRetrieval, OrbitError> {
        validate_precedent_request(request).map_err(|reason| {
            OrbitError::InvalidInput(format!("invalid precedent query: {reason:?}"))
        })?;
        if request.workspace_id != self.workspace_id()? {
            return Err(OrbitError::InvalidInput(
                "precedent query must name the current workspace".into(),
            ));
        }
        let settings = orbit_config::ConfigStore::open(
            orbit_config::ConfigScope::Global,
            self.global_root().join("config.toml"),
        )?
        .snapshot()?
        .assistance_settings;
        let Some(policy) = settings
            .precedent_providers
            .iter()
            .find(|policy| policy.workspace_id == request.workspace_id)
        else {
            return Ok(PrecedentRetrieval::default());
        };
        let mut result = PrecedentRetrieval {
            enabled: true,
            ..Default::default()
        };
        let mut sources = Vec::new();
        if policy.built_in {
            sources.push((
                "orbit-decision-history".to_string(),
                self.built_in_decision_precedents(request),
            ));
        }
        for name in &policy.plugin_tools {
            sources.push((name.clone(), self.plugin_decision_precedents(name, request)));
        }
        for (name, source) in sources {
            match source {
                Ok(mut response) => {
                    response.precedents.sort_by(|a, b| a.id.cmp(&b.id));
                    result.precedents.extend(response.precedents);
                    result.omissions.extend(
                        response
                            .omissions
                            .into_iter()
                            .map(|omission| format!("{name}: {omission}")),
                    );
                    result.truncated |= response.truncated;
                }
                Err(reason) => {
                    system::record_system_audit_event(
                        self,
                        SystemAuditEvent {
                            execution_id_prefix: "precedent-gap",
                            command: "decision",
                            subcommand: Some("precedent_provider_gap"),
                            tool_name: Some(&name),
                            target_type: "decision_request",
                            target_id: Some(&request.request.id),
                            role: "system",
                            status: AuditEventStatus::Denied,
                            arguments_json: serde_json::json!({"provider": name, "reason": reason})
                                .to_string(),
                            workspace_id: Some(&request.workspace_id),
                            task_id: None,
                            attribution: AuditRunAttribution::Environment { run_id: None },
                        },
                    )?;
                    result.provider_gaps.push(PrecedentProviderGap {
                        provider: name,
                        reason,
                    });
                }
            }
        }
        merge_decision_precedents(&mut result.precedents, request.k);
        Ok(result)
    }

    fn built_in_decision_precedents(
        &self,
        request: &PrecedentProviderRequest,
    ) -> Result<PrecedentProviderResponse, PrecedentGapReason> {
        let tasks = self
            .list_task_metadata()
            .map_err(|_| PrecedentGapReason::Unavailable)?;
        let mut records = Vec::new();
        for task in tasks {
            let history = self
                .get_task_history(&task.id)
                .map_err(|_| PrecedentGapReason::Unavailable)?;
            records.extend(history::decision_precedents_from_history(request, &history));
            // Keep scan memory bounded by k plus one task's history.
            history::rank_history_precedents(request, &mut records);
        }
        let response = PrecedentProviderResponse {
            contract: PRECEDENT_PROVIDER_CONTRACT.into(),
            precedents: records,
            omissions: Vec::new(),
            truncated: false,
        };
        let bytes = serde_json::to_vec(&response).map_err(|_| PrecedentGapReason::InvalidShape)?;
        validate_precedent_response(request, &bytes)
    }

    fn plugin_decision_precedents(
        &self,
        name: &str,
        request: &PrecedentProviderRequest,
    ) -> Result<PrecedentProviderResponse, PrecedentGapReason> {
        // A configured name must resolve to an active read-only plugin tool,
        // never a similarly named builtin or a mutating plugin command.
        let read_only = self.plugin_load().active().any(|plugin| {
            plugin.tools.iter().any(|tool| {
                tool.execution_kind == PluginExecutionKind::ReadOnly
                    && plugin_tool_name(
                        plugin.namespace(),
                        &tool.verb,
                        plugin.manifest.claims_first_party_namespace(),
                    ) == name
            })
        });
        if !read_only {
            return Err(PrecedentGapReason::Unavailable);
        }
        // Normal audited dispatch sets up the plugin's existing callback
        // session and sandbox/grants. Never execute a backend directly.
        let value = self
            .execute_tool_command_with_session_context(
                name,
                serde_json::to_value(request).map_err(|_| PrecedentGapReason::InvalidShape)?,
                None,
                None,
                ToolSessionContext {
                    effective_capabilities: [McpCapability::Agent].into_iter().collect(),
                    ..Default::default()
                },
            )
            .map_err(|_| PrecedentGapReason::Unavailable)?;
        let bytes = serde_json::to_vec(&value).map_err(|_| PrecedentGapReason::InvalidShape)?;
        validate_precedent_response(request, &bytes)
    }
}

/// Recency half-life is monotonic in `decided_at`, so descending timestamps
/// give exactly the same order for every positive half-life (default 90 days).
/// Stable sorting preserves configuration source order for equal weights.
pub fn merge_decision_precedents(records: &mut Vec<DecisionPrecedent>, k: usize) {
    // A duplicate citation that any source reports reversed remains a
    // contradiction, whichever source wins deduplication.
    let reversed: BTreeSet<_> = records
        .iter()
        .filter(|record| record.reversed)
        .map(|record| record.citation.digest.clone())
        .collect();
    records.sort_by_key(|record| std::cmp::Reverse(record.decided_at));
    let mut digests = BTreeSet::new();
    records.retain(|record| digests.insert(record.citation.digest.clone()));
    for record in records.iter_mut() {
        record.reversed |= reversed.contains(&record.citation.digest);
    }
    records.truncate(k.min(MAX_PRECEDENT_K));
}
