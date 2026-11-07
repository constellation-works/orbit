//! Projections of the admitted [`ConfigSnapshot`] into the typed settings
//! the domain modules own. They sit beside the snapshot, above the domain
//! admission modules the registry calls, so no domain module names the
//! registry [ORB-22707].

use std::collections::BTreeMap;

use super::ConfigSnapshot;
use crate::attention::AttentionSettings;
use crate::crew_ladder::{CrewLadderMode, CrewLadderSettings};
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
    /// The admitted `workflow.crew_ladder.*` settings.
    #[must_use]
    pub fn crew_ladder(&self) -> CrewLadderSettings {
        CrewLadderSettings {
            mode: CrewLadderMode::parse(&self.workflow_crew_ladder_mode).unwrap_or_default(),
            rungs: self.workflow_crew_ladder_rungs.clone(),
            tiers: self
                .workflow_crew_ladder_tiers
                .iter()
                .filter_map(|tier| tier.parse().ok())
                .collect(),
            explore_rate: self.workflow_crew_ladder_explore_rate,
            margin: self.workflow_crew_ladder_margin,
            look_every: self.workflow_crew_ladder_look_every,
            max_sample: self.workflow_crew_ladder_max_sample,
            exclude_types: self
                .workflow_crew_ladder_exclude_types
                .iter()
                .filter_map(|name| name.parse().ok())
                .collect(),
            watch_window: self.workflow_crew_ladder_watch_window,
            retry_after_days: self.workflow_crew_ladder_retry_after_days,
            quality_grade_margin: self.workflow_crew_ladder_quality_grade_margin,
            min_grade_coverage: self.workflow_crew_ladder_min_grade_coverage,
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
