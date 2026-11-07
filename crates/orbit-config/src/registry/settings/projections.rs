//! Projections of the admitted [`ConfigSnapshot`] into the typed settings
//! the domain modules own. They sit beside the snapshot, above the domain
//! admission modules the registry calls, so no domain module names the
//! registry [ORB-22707].

use std::collections::BTreeMap;

use super::ConfigSnapshot;
use crate::attention::AttentionSettings;
use crate::crew_task_types::TaskTypeDefinition;
use crate::provider_limit::{
    CrewSelection, ProviderLimitBudget, ProviderLimitExplicitCrews, ProviderLimitPolicy,
    ProviderLimitTarget, UsagePressurePolicy, stored_provider_limit_override,
};

impl AttentionSettings {
    /// Read the canonical admitted registry values.
    pub fn from_snapshot(snapshot: &ConfigSnapshot) -> Self {
        Self {
            cursor_ttl_hours: snapshot.attention_cursor_ttl_hours,
            run_window_hours: snapshot.attention_run_window_hours,
            run_stuck_after_minutes: snapshot.attention_run_stuck_after_minutes,
            ci_cache_seconds: snapshot.attention_ci_cache_seconds,
            source_timeout_seconds: snapshot.attention_source_timeout_seconds,
            fanout_budget_seconds: snapshot.attention_fanout_budget_seconds,
            per_workspace_item_cap: snapshot.attention_per_workspace_item_cap,
            age_bump_hours: snapshot.attention_age_bump_hours.clone(),
        }
    }
}

impl ConfigSnapshot {
    /// The admitted `workflow.task_types`, in configured order.
    #[must_use]
    pub fn task_types(&self) -> &[TaskTypeDefinition] {
        &self.workflow_task_types
    }
}

impl ConfigSnapshot {
    /// The admitted `workflow.provider_limit_budgets`.
    #[must_use]
    pub fn provider_limit_budgets(&self) -> Vec<ProviderLimitBudget> {
        self.workflow_provider_limit_budgets
            .iter()
            .filter_map(|entry| ProviderLimitBudget::parse(entry).ok())
            .collect()
    }
}

impl ConfigSnapshot {
    /// The admitted `workflow.provider_limit_*` settings.
    #[must_use]
    pub fn provider_limit_policy(&self) -> ProviderLimitPolicy {
        let mut overrides = BTreeMap::new();
        let mut crew_overrides = BTreeMap::new();
        for (target, threshold) in self
            .workflow_provider_limit_overrides
            .iter()
            .filter_map(|entry| stored_provider_limit_override(entry))
        {
            match target {
                ProviderLimitTarget::Provider(provider) => {
                    overrides.insert(provider, threshold);
                }
                ProviderLimitTarget::Crew(crew) => {
                    crew_overrides.insert(crew, threshold);
                }
            }
        }
        ProviderLimitPolicy {
            max_used_pct: self.workflow_provider_limit_max_used_pct,
            overrides,
            crew_overrides,
            explicit_crews: ProviderLimitExplicitCrews::parse(
                &self.workflow_provider_limit_explicit_crews,
            )
            .unwrap_or_default(),
            budgets: self.provider_limit_budgets(),
            usage_pressure: UsagePressurePolicy {
                selection: CrewSelection::parse(&self.workflow_crew_selection).unwrap_or_default(),
                cap: self.workflow_usage_pressure_cap,
                strength: self.workflow_usage_pressure_strength,
            },
        }
    }
}
