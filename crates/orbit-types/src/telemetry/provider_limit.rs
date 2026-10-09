//! What a host knows about its provider accounts' usage limits [ORB-14695].
//!
//! A usage limit belongs to the provider login on one host, so each host keeps
//! its own record. Every limit failure writes an observation, and so does
//! every Codex or Claude run that reported its usage windows [ORB-14696]; the
//! latest per provider, model scope and window stands. A provider that reports
//! nothing can instead be given an operator-declared budget, which the host
//! reads from its invocation ledger as a `ledger` observation [ORB-14699];
//! those are computed when asked for and never stored.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Largest detail an observation keeps, in bytes.
pub const PROVIDER_LIMIT_DETAIL_MAX_BYTES: usize = 512;

/// The providers whose own output reports their usage windows after a run
/// [ORB-14696]. Every other provider is read only from a limit failure.
pub const USAGE_REPORTING_PROVIDERS: [&str; 2] = ["claude", "codex"];

/// Where an observation came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderLimitSource {
    /// The provider failed a run because the limit was reached.
    Error,
    /// The provider reported a window's usage in its own telemetry after a
    /// run, whether or not the run failed [ORB-14696].
    Event,
    /// The operator's declared budget for a provider that reports no usage,
    /// read from this host's invocation ledger [ORB-14699]. Computed, never
    /// stored.
    Ledger,
}

impl ProviderLimitSource {
    /// The source's wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Event => "event",
            Self::Ledger => "ledger",
        }
    }

    /// The source a wire name names.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "error" => Some(Self::Error),
            "event" => Some(Self::Event),
            "ledger" => Some(Self::Ledger),
            _ => None,
        }
    }
}

/// One reading of a provider account's usage limit on this host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderLimitObservation {
    /// The provider whose account the limit belongs to (`codex`, `claude`).
    pub provider: String,
    /// The model or model family the limit covers, when the provider named
    /// one; `None` covers the whole account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The limit's window as the provider labels it, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    /// Whether the limit was reached.
    pub exhausted: bool,
    pub source: ProviderLimitSource,
    /// When the provider said the limit resets; `None` when it did not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<DateTime<Utc>>,
    /// How much of the window the account has used, in percent, as the
    /// provider reported it. It can exceed 100. `None` when the provider did
    /// not say, as a limit failure usually does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_percent: Option<f64>,
    /// The window's length in minutes, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_minutes: Option<u32>,
    /// Whether reaching this window stops the account's runs. An overage
    /// window is recorded for display but does not gate.
    #[serde(default = "gating_default")]
    pub gating: bool,
    /// Whether the reading undercounts: a `usd` budget's ledger sum leaves
    /// out invocations that recorded no cost [ORB-14699].
    #[serde(default, skip_serializing_if = "is_false")]
    pub partial: bool,
    pub observed_at: DateTime<Utc>,
    /// The run that observed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The crew the run used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    /// The provider's own words, redacted and bounded to
    /// [`PROVIDER_LIMIT_DETAIL_MAX_BYTES`].
    pub detail: String,
}

fn gating_default() -> bool {
    true
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl ProviderLimitObservation {
    /// `detail` cut to [`PROVIDER_LIMIT_DETAIL_MAX_BYTES`] on a character
    /// boundary.
    #[must_use]
    pub fn bounded_detail(detail: &str) -> String {
        let detail = detail.trim();
        let mut cut = detail.len().min(PROVIDER_LIMIT_DETAIL_MAX_BYTES);
        while !detail.is_char_boundary(cut) {
            cut -= 1;
        }
        detail[..cut].to_string()
    }
}
