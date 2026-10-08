//! A task held back from the provider that just failed it [ORB-14266].
//!
//! A local run that fails because its provider was at capacity, unusable on
//! the host, or refused the task's content did not judge the work. Its task
//! goes back to the `backlog` under a [`PROVIDER_FAILURE_HOLD_EVENT`] whose
//! note carries a typed [`ProviderFailureHold`]: the crews it excludes and the
//! time they may run the task again. Until then admission draws the task's
//! crew from what is left — another member of its complexity pool, or the
//! workspace default — and defers the task when nothing is left.
//!
//! The note carries the hold as JSON straight after
//! [`PROVIDER_FAILURE_HOLD_MARKER`], so the writer (run finalization) and the
//! readers (admission, the next hold) share one text form.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::state::{
    PROVIDER_CAPACITY_MARKER, PROVIDER_REFUSAL_MARKER, PROVIDER_UNAVAILABLE_MARKER,
    is_provider_capacity_exhausted, is_provider_refusal, is_provider_unavailable,
};

/// Task history event that moves a task to `backlog` under a
/// [`ProviderFailureHold`].
pub const PROVIDER_FAILURE_HOLD_EVENT: &str = "provider_failure_hold";

/// The marker a hold's history note starts with, followed by the hold JSON.
pub const PROVIDER_FAILURE_HOLD_MARKER: &str = "[provider_failure_hold]";

/// The token naming the failing provider right after a provider marker in a
/// step failure: `[provider_refusal] provider=codex …`.
const PROVIDER_TOKEN: &str = "provider=";

/// Which provider failure ended the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFailureClass {
    /// The selected model had no capacity.
    Capacity,
    /// The provider could not be used on this host (authentication, say).
    Unavailable,
    /// The provider's content policy refused the task.
    Refusal,
}

impl ProviderFailureClass {
    /// The class a step failure's code or message carries, if any. Capacity
    /// is checked before unavailability, which it is a kind of.
    #[must_use]
    pub fn of(error_code: Option<&str>, message: Option<&str>) -> Option<Self> {
        if is_provider_refusal(error_code, message) {
            Some(Self::Refusal)
        } else if is_provider_capacity_exhausted(error_code, message) {
            Some(Self::Capacity)
        } else if is_provider_unavailable(error_code, message) {
            Some(Self::Unavailable)
        } else {
            None
        }
    }

    /// The bracketed marker a step failure of this class carries.
    #[must_use]
    pub fn marker(self) -> &'static str {
        match self {
            Self::Capacity => PROVIDER_CAPACITY_MARKER,
            Self::Unavailable => PROVIDER_UNAVAILABLE_MARKER,
            Self::Refusal => PROVIDER_REFUSAL_MARKER,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Capacity => "provider_capacity",
            Self::Unavailable => "provider_unavailable",
            Self::Refusal => "provider_refusal",
        }
    }
}

/// `[marker] provider=<provider> <detail>`: the step failure text of a
/// provider failure, naming the provider the hold excludes.
#[must_use]
pub fn provider_failure_text(class: ProviderFailureClass, provider: &str, detail: &str) -> String {
    format!(
        "{} {PROVIDER_TOKEN}{provider} {}",
        class.marker(),
        detail.trim()
    )
}

/// The provider a step failure names after its provider marker, if it does.
#[must_use]
pub fn failed_provider(message: &str) -> Option<&str> {
    let class = ProviderFailureClass::of(None, Some(message))?;
    let (_, rest) = message.split_once(class.marker())?;
    let name = rest.trim_start().strip_prefix(PROVIDER_TOKEN)?;
    let name = name.split(|c: char| c.is_whitespace() || c == ':').next()?;
    (!name.is_empty()).then_some(name)
}

/// The crews a task may not run as until `not_before`, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderFailureHold {
    pub class: ProviderFailureClass,
    /// The provider that failed, when the run named it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Crews admission does not draw for the task before `not_before`.
    pub excluded_crews: Vec<String>,
    /// When the excluded crews may run the task again.
    pub not_before: DateTime<Utc>,
    /// The run whose failure placed the hold.
    pub run_id: String,
}

impl ProviderFailureHold {
    /// `detail` prefixed with the marker and this hold, the history note form.
    #[must_use]
    pub fn text(&self, detail: &str) -> String {
        let hold = serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string());
        format!("{PROVIDER_FAILURE_HOLD_MARKER} {hold} {}", detail.trim())
    }

    /// The hold a history note carries, if any. A hold may exclude no crew
    /// (the failed run resolved none); its backoff still stands.
    #[must_use]
    pub fn from_text(text: &str) -> Option<Self> {
        let (_, rest) = text.split_once(PROVIDER_FAILURE_HOLD_MARKER)?;
        serde_json::Deserializer::from_str(rest.trim_start())
            .into_iter::<Self>()
            .next()?
            .ok()
    }

    /// Whether the hold still excludes its crews at `now`.
    #[must_use]
    pub fn stands_at(&self, now: DateTime<Utc>) -> bool {
        now < self.not_before
    }

    /// Whether `crew` is excluded by this hold.
    #[must_use]
    pub fn excludes(&self, crew: &str) -> bool {
        self.excluded_crews.iter().any(|excluded| excluded == crew)
    }
}
