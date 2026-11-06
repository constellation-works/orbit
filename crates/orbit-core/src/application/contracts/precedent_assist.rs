//! Pure records of a precedent-answered category's enablement [ORB-22313].
//!
//! `task`'s decision digest reports demoted categories and the precedent
//! consumer above it stores and updates them, so neither may own the shape
//! without pulling the other into a dependency on itself. The behavior that
//! reads a live assistance grant stays with the consumer.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Where an enablement stands. Only `enabled` lets the model answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrecedentAssistState {
    /// The operator enabled the category under the live grant.
    Enabled,
    /// The operator disabled it.
    Disabled,
    /// The rolling window fell below its bar or reached `max_vetoes`.
    Demoted,
    /// The live grant's digest or model revision changed.
    Suspended,
}

impl PrecedentAssistState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::Demoted => "demoted",
            Self::Suspended => "suspended",
        }
    }
}

/// Where a scored live outcome came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrecedentAssistOutcomeSource {
    /// A holdback prediction scored against the human's answer.
    Holdback,
    /// An excluded-tier prediction scored against the human's answer.
    ExcludedTier,
    /// A model answer a human confirmed.
    Confirmed,
    /// A model answer a human vetoed.
    Vetoed,
}

/// One scored live outcome in the rolling window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrecedentAssistOutcome {
    pub request_id: String,
    pub at: DateTime<Utc>,
    pub source: PrecedentAssistOutcomeSource,
    pub agreed: bool,
}

/// What the rolling window held when it demoted the category.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrecedentAssistDemotion {
    /// `accuracy` (Wilson lower bound below `min_accuracy`) or `max_vetoes`.
    pub trigger: String,
    pub outcomes: u32,
    pub agreed: u32,
    pub vetoes: u32,
    pub wilson_lower: Option<f64>,
    pub min_accuracy: f64,
    pub max_vetoes: u32,
    pub demotion_window: u32,
    /// The window's request ids, oldest first.
    pub request_ids: Vec<String>,
}

/// Why an enablement stopped answering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrecedentAssistEnd {
    pub at: DateTime<Utc>,
    /// The operator who disabled it, or `system`.
    pub by: String,
    /// `disabled`, `demoted`, `grant_changed`, `model_revision_changed` or
    /// `grant_withdrawn`.
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub demotion: Option<PrecedentAssistDemotion>,
}

/// One category's enablement record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrecedentAssistEnablement {
    pub category: String,
    pub state: PrecedentAssistState,
    pub policy_version: String,
    pub policy_digest: String,
    pub model_revision: String,
    pub threshold: f64,
    pub min_accuracy: f64,
    /// The readout it was enabled from, as `<task>/<artifact>`.
    pub readout: String,
    pub operator: String,
    pub enabled_at: DateTime<Utc>,
    pub assist_wait_seconds: u64,
    pub holdback_share: f64,
    pub demotion_window: u32,
    pub max_vetoes: u32,
    #[serde(default)]
    pub window: Vec<PrecedentAssistOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<PrecedentAssistEnd>,
}

// Thresholds and bars come from a checked readout and the window's Wilson
// bound is finite, so no stored record holds a NaN and equality is total.
impl Eq for PrecedentAssistEnablement {}
