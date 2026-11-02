//! Capability gaps and their measured costs, separate from repair friction.
use crate::workflow::ResidentCostEvent;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvolutionState {
    Open,
    Proposed,
    Declined,
    Resolved,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvolutionOccurrence {
    pub at: DateTime<Utc>,
    pub mandate: Option<String>,
    pub cycle: Option<String>,
    /// Stable checkpoint/index key, or an interactive event identifier.
    pub source_key: String,
    #[serde(flatten)]
    pub cost: ResidentCostEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename = "evolution")]
pub struct EvolutionRecord {
    pub id: String,
    pub workspace: String,
    pub signature: String,
    pub title: String,
    pub wished: Option<String>,
    pub state: EvolutionState,
    pub occurrences: Vec<EvolutionOccurrence>,
    pub proposal_task: Option<String>,
    pub declined_until: Option<DateTime<Utc>>,
    pub cost_at_decline: u64,
    pub window_days: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct EvolutionTotals {
    pub occurrence_count: usize,
    pub distinct_cycles: Vec<String>,
    pub distinct_mandates: Vec<String>,
    pub cycle_count: usize,
    pub mandate_count: usize,
    pub minutes: u64,
    pub retries: u64,
    pub slot_minutes: u64,
}

impl EvolutionRecord {
    /// Derived aggregates; a window never changes the retained occurrences.
    pub fn evolution_totals(&self, since: Option<DateTime<Utc>>) -> EvolutionTotals {
        let mut totals = EvolutionTotals::default();
        let mut cycles = BTreeSet::new();
        let mut mandates = BTreeSet::new();
        for event in self
            .occurrences
            .iter()
            .filter(|event| since.is_none_or(|at| event.at >= at))
        {
            totals.occurrence_count += 1;
            if let Some(cycle) = &event.cycle {
                cycles.insert(cycle.clone());
            }
            if let Some(mandate) = &event.mandate {
                mandates.insert(mandate.clone());
            }
            totals.minutes = totals.minutes.saturating_add(event.cost.minutes);
            totals.retries = totals.retries.saturating_add(event.cost.retries);
            totals.slot_minutes = totals.slot_minutes.saturating_add(event.cost.slot_minutes);
        }
        totals.distinct_cycles = cycles.into_iter().collect();
        totals.distinct_mandates = mandates.into_iter().collect();
        totals.cycle_count = totals.distinct_cycles.len();
        totals.mandate_count = totals.distinct_mandates.len();
        totals
    }
}
