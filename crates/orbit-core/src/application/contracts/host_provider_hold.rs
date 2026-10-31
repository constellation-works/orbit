//! A standing provider failure hold on this host. `distributed` projects
//! the holds from run history and job's crew draws skip the crews they
//! cover, so neither owns the shape [ORB-17937].

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_types::identity::Crew;
use orbit_types::workflow::{Provider, ProviderFailureClass, provider_account_label};

/// One standing unavailable or capacity hold on this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostProviderHold {
    pub(crate) class: ProviderFailureClass,
    /// Canonical provider (`claude`, `codex`).
    pub(crate) provider: String,
    /// The crew account that failed, when the failure named one; `None` is
    /// the provider's default login.
    pub(crate) account: Option<String>,
    /// `None` covers every crew of `provider` on `account`. A capacity hold
    /// names the run's crew when that crew's provider is the one that failed.
    pub(crate) crew: Option<String>,
    pub(crate) not_before: DateTime<Utc>,
    pub(crate) run_id: String,
}

impl HostProviderHold {
    /// Whether `crew` is excluded at `now`.
    pub(crate) fn covers(&self, crew: &Crew, now: DateTime<Utc>) -> bool {
        if now >= self.not_before {
            return false;
        }
        if canonical_provider(&crew.assignment.provider) != self.provider {
            return false;
        }
        match self.crew.as_deref() {
            Some(name) => name == crew.name,
            None => crew.account() == self.account.as_deref(),
        }
    }

    /// `provider_unavailable <subject> until <time> (<run>)`. The subject is
    /// the crew when the hold names one, otherwise the provider account.
    pub(crate) fn detail(&self) -> String {
        let account = provider_account_label(&self.provider, self.account.as_deref());
        let subject = self.crew.as_deref().unwrap_or(account.as_str());
        let until = self.not_before.to_rfc3339_opts(SecondsFormat::Secs, true);
        format!(
            "{} {subject} until {until} ({})",
            self.class.as_str(),
            self.run_id
        )
    }
}

/// The canonical name of provider `raw`, or `raw` lowercased when it names
/// no known provider.
pub(crate) fn canonical_provider(raw: &str) -> String {
    let trimmed = raw.trim();
    Provider::parse(trimmed)
        .map(|provider| provider.as_str().to_string())
        .unwrap_or_else(|_| trimmed.to_ascii_lowercase())
}
