//! Operator-declared rolling budgets for providers that report no usage
//! (`workflow.provider_limit_budgets`) [ORB-14699].
//!
//! A budget is the operator's share of a provider's allowance on this host:
//! an amount of `usd` or `tokens` per rolling window, written
//! `provider:<amount><unit>/<n><h|d>`, like `grok:30usd/5h`. The host reads
//! its spend against the budget from its invocation ledger; this module only
//! parses and admits the declarations.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::workflow::Provider;

use crate::ConfigSnapshot;

pub(crate) const BUDGETS_KEY: &str = "workflow.provider_limit_budgets";

/// The longest window a budget may roll over. It bounds the ledger read.
const MAX_WINDOW_HOURS: u32 = 31 * 24;

/// What a budget's amount counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderLimitBudgetUnit {
    /// Provider-reported cost in US dollars. Invocations that recorded no
    /// cost count for nothing.
    Usd,
    /// Input plus output tokens, as the invocation ledger totals them.
    Tokens,
}

impl ProviderLimitBudgetUnit {
    /// The unit as written in a budget entry.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Usd => "usd",
            Self::Tokens => "tokens",
        }
    }
}

/// One admitted `workflow.provider_limit_budgets` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderLimitBudget {
    /// The canonical provider the budget covers.
    pub provider: String,
    /// The amount the window may spend, in `unit`. Positive.
    pub amount: f64,
    /// What `amount` counts.
    pub unit: ProviderLimitBudgetUnit,
    /// The rolling window's length, in hours.
    pub window_hours: u32,
    /// The window as written, `5h` or `7d`.
    pub window_label: String,
}

impl ProviderLimitBudget {
    /// The window's length in minutes.
    #[must_use]
    pub fn window_minutes(&self) -> u32 {
        self.window_hours * 60
    }

    /// The entry in its stored form, `grok:30usd/5h`.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "{}:{}{}/{}",
            self.provider,
            self.amount,
            self.unit.as_str(),
            self.window_label
        )
    }

    /// Parse one entry: the provider a known one (an alias resolves to its
    /// canonical name), the amount a positive number (a whole number of
    /// tokens), the window `<n>h` or `<n>d` of at most 31 days.
    pub fn parse(entry: &str) -> Result<Self, String> {
        let shape = || {
            format!(
                "entry `{entry}` must be written `provider:<amount><usd|tokens>/<n><h|d>`, \
                 like `grok:30usd/5h`"
            )
        };
        let (provider, rest) = entry.trim().split_once(':').ok_or_else(shape)?;
        let (amount, window) = rest.split_once('/').ok_or_else(shape)?;
        let provider = Provider::parse(provider.trim())
            .map_err(|error| format!("entry `{entry}`: {error}"))?;

        let amount = amount.trim().to_ascii_lowercase();
        let (number, unit) = if let Some(number) = amount.strip_suffix("usd") {
            (number, ProviderLimitBudgetUnit::Usd)
        } else if let Some(number) = amount.strip_suffix("tokens") {
            (number, ProviderLimitBudgetUnit::Tokens)
        } else {
            return Err(format!(
                "entry `{entry}` must give its amount in `usd` or `tokens`"
            ));
        };
        let number = number.trim();
        let digits =
            |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
        let plain = match unit {
            ProviderLimitBudgetUnit::Usd => match number.split_once('.') {
                Some((whole, cents)) => digits(whole) && digits(cents),
                None => digits(number),
            },
            ProviderLimitBudgetUnit::Tokens => digits(number),
        };
        let amount = number
            .parse::<f64>()
            .ok()
            .filter(|amount| plain && amount.is_finite() && *amount > 0.0)
            .ok_or_else(|| {
                let what = match unit {
                    ProviderLimitBudgetUnit::Usd => "a positive number of dollars",
                    ProviderLimitBudgetUnit::Tokens => "a positive whole number of tokens",
                };
                format!("entry `{entry}` must give {what}")
            })?;

        let window = window.trim().to_ascii_lowercase();
        let window_error = || {
            format!(
                "entry `{entry}` must give a window of `<n>h` or `<n>d`, at most {} days",
                MAX_WINDOW_HOURS / 24
            )
        };
        let (count, per_unit) = if let Some(count) = window.strip_suffix('h') {
            (count, 1)
        } else if let Some(count) = window.strip_suffix('d') {
            (count, 24)
        } else {
            return Err(window_error());
        };
        let window_hours = Some(count)
            .filter(|count| digits(count))
            .and_then(|count| count.parse::<u32>().ok())
            .filter(|count| *count > 0)
            .and_then(|count| count.checked_mul(per_unit))
            .filter(|hours| *hours <= MAX_WINDOW_HOURS)
            .ok_or_else(window_error)?;

        Ok(Self {
            provider: provider.as_str().to_string(),
            amount,
            unit,
            window_hours,
            window_label: window,
        })
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

/// Admit `workflow.provider_limit_budgets`: every entry
/// `provider:<amount><unit>/<window>`, each provider named once (an alias is
/// stored under its canonical name). Stored sorted by provider; none by
/// default.
pub(crate) fn admit_budgets(raw: Option<Vec<String>>) -> Result<Vec<String>, OrbitError> {
    let mut budgets = BTreeMap::new();
    for entry in raw.unwrap_or_default() {
        let budget = ProviderLimitBudget::parse(&entry)
            .map_err(|reason| OrbitError::InvalidInput(format!("{BUDGETS_KEY} {reason}")))?;
        let provider = budget.provider.clone();
        if budgets.insert(provider.clone(), budget).is_some() {
            return Err(OrbitError::InvalidInput(format!(
                "{BUDGETS_KEY} names provider `{provider}` more than once"
            )));
        }
    }
    Ok(budgets.values().map(ProviderLimitBudget::render).collect())
}
