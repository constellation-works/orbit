//! Readings of operator-declared provider budgets from this host's
//! invocation ledger [ORB-14699].
//!
//! A provider that cannot report how close it is to its limit (grok, say) can
//! be given a rolling budget, `workflow.provider_limit_budgets`. The reading
//! is the host's spend on that provider in the trailing window as a percent
//! of the budget, a `ledger` observation that admission and every surface
//! consume like a provider's own. It is computed when asked for, never
//! stored, and yields to a live stored reading of the same provider: a
//! provider-reported window or a limit failure.
//!
//! The ledger holds only Orbit's own runs on this host. Interactive use and
//! other hosts sharing the login are invisible to it, so a budget is the
//! operator's share of the allowance, not the allowance.

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_config::{ProviderLimitBudget, ProviderLimitBudgetUnit};
use orbit_store::contracts::ProviderLedgerEntry;
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};
use orbit_types::workflow::Provider;

use super::provider_limit::reading_lifetime;
use crate::OrbitRuntime;
use crate::runtime::task::provider_hold::same_provider;

/// What a surface adds to a reading that undercounts.
pub(crate) const PARTIAL_NOTE: &str = "partial: some invocations report no cost";

/// The ledger reading of `budget` at `now` over `entries`, the provider's
/// invocations in the trailing window. `threshold` is the provider's gating
/// threshold, which decides when the reading's reset falls.
pub(crate) fn ledger_observation(
    budget: &ProviderLimitBudget,
    entries: &[ProviderLedgerEntry],
    threshold: u8,
    now: DateTime<Utc>,
) -> ProviderLimitObservation {
    let window = Duration::hours(budget.window_hours.into());
    let in_window = entries
        .iter()
        .filter(|entry| entry.ts + window > now && entry.ts <= now)
        .collect::<Vec<_>>();
    let amount_of = |entry: &ProviderLedgerEntry| match budget.unit {
        ProviderLimitBudgetUnit::Usd => entry.cost_usd.unwrap_or(0.0).max(0.0),
        ProviderLimitBudgetUnit::Tokens => entry.tokens as f64,
    };
    let partial = budget.unit == ProviderLimitBudgetUnit::Usd
        && in_window.iter().any(|entry| entry.cost_usd.is_none());
    let used_pct = |spent: f64| spent * 100.0 / budget.amount;
    let spent: f64 = in_window.iter().map(|entry| amount_of(entry)).sum();
    let used = used_pct(spent);

    // The reading stops gating once enough of the oldest spend ages out.
    let at_limit = |used: f64| used >= 100.0 || (threshold < 100 && used >= f64::from(threshold));
    let resets_at = at_limit(used).then(|| {
        let mut remaining = spent;
        let mut resets_at = now + window;
        for entry in in_window.iter().filter(|entry| amount_of(entry) > 0.0) {
            remaining -= amount_of(entry);
            resets_at = entry.ts + window;
            if !at_limit(used_pct(remaining.max(0.0))) {
                break;
            }
        }
        resets_at
    });

    let spent_text = match budget.unit {
        ProviderLimitBudgetUnit::Usd => format!("${spent:.2} of ${}", budget.amount),
        ProviderLimitBudgetUnit::Tokens => {
            format!("{spent} of {} tokens", budget.amount)
        }
    };
    ProviderLimitObservation {
        provider: budget.provider.clone(),
        model: None,
        window: Some(format!("{} budget", budget.window_label)),
        exhausted: used >= 100.0,
        source: ProviderLimitSource::Ledger,
        resets_at,
        used_percent: Some(used),
        window_minutes: Some(budget.window_minutes()),
        gating: true,
        partial,
        observed_at: now,
        run_id: None,
        crew: None,
        detail: ProviderLimitObservation::bounded_detail(&format!(
            "{spent_text} in the trailing {} on this host{}",
            budget.window_label,
            if partial { " (partial)" } else { "" }
        )),
    }
}

/// The names `provider` goes by in the ledger: its canonical name and its
/// aliases.
fn ledger_names(provider: Provider) -> Vec<String> {
    std::iter::once(provider.as_str().to_string())
        .chain(
            Provider::ALIASES
                .iter()
                .filter(|alias| alias.canonical == provider)
                .map(|alias| alias.alias.to_string()),
        )
        .collect()
}

impl OrbitRuntime {
    /// The provider limits in force at `now`: this host's stored readings,
    /// and a ledger reading for each budgeted provider with no live stored
    /// reading. A provider-reported window or a limit failure takes
    /// precedence over the ledger, whatever it says. A budget whose ledger
    /// cannot be read is logged and gates nothing, as an unreadable store
    /// does.
    pub(crate) fn provider_limit_observations(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<ProviderLimitObservation>, OrbitError> {
        let mut observations = self.provider_limits()?;
        let policy = self.context.settings().provider_limit();
        for budget in &policy.budgets {
            let Ok(provider) = Provider::parse(&budget.provider) else {
                continue;
            };
            let reported = observations.iter().any(|observation| {
                same_provider(Some(&observation.provider), Some(&budget.provider))
                    && reading_lifetime(observation) > now
            });
            if reported {
                continue;
            }
            let since = now - Duration::hours(budget.window_hours.into());
            match self.provider_ledger_entries(&ledger_names(provider), since) {
                Ok(entries) => observations.push(ledger_observation(
                    budget,
                    &entries,
                    policy.threshold(&budget.provider),
                    now,
                )),
                Err(error) => tracing::warn!(
                    "could not read this host's invocation ledger for the {} budget: {error}",
                    budget.provider
                ),
            }
        }
        Ok(observations)
    }
}
