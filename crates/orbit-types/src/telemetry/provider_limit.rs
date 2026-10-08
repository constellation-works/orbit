//! What a host knows about its provider accounts' usage limits [ORB-14695].
//!
//! A usage limit belongs to the provider login on one host, so each host keeps
//! its own record. Every limit failure writes an observation; the latest per
//! provider, model scope and window stands.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Largest detail an observation keeps, in bytes.
pub const PROVIDER_LIMIT_DETAIL_MAX_BYTES: usize = 512;

/// Where an observation came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderLimitSource {
    /// The provider failed a run because the limit was reached.
    Error,
}

impl ProviderLimitSource {
    /// The source's wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
        }
    }

    /// The source a wire name names.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// One reading of a provider account's usage limit on this host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
