//! A task held back from the provider that just failed it [ORB-14266].
//!
//! A local run that fails because its provider was at capacity, unusable on
//! the host, out of its account's usage limit [ORB-14695], or refused the
//! task's content did not judge the work. Its task
//! goes back to the `backlog` under a [`PROVIDER_FAILURE_HOLD_EVENT`] whose
//! note carries a typed [`ProviderFailureHold`]: the crews it excludes and the
//! time they may run the task again. Until then admission draws the task's
//! crew from what is left — another member of its complexity pool, or the
//! workspace default — and defers the task when nothing is left.
//!
//! The note carries the hold as JSON straight after
//! [`PROVIDER_FAILURE_HOLD_MARKER`], so the writer (run finalization) and the
//! readers (admission, the next hold) share one text form.
//!
//! [ORB-16379] An unavailable provider is the failing host's: its login
//! expired there, not on every machine. Such a hold names that `machine` and
//! excludes its crews only there. Capacity, limit and refusal holds name none
//! and exclude their crews on every machine, as does a hold stored before
//! holds named a machine. The host itself also records a
//! [`ProviderCrewOutage`] for the failed crew, which keeps its own drain off
//! that crew for every task until the outage lapses.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::state::{
    PROVIDER_CAPACITY_MARKER, PROVIDER_LIMIT_MARKER, PROVIDER_REFUSAL_MARKER,
    PROVIDER_UNAVAILABLE_MARKER, is_provider_capacity_exhausted, is_provider_limit,
    is_provider_refusal, is_provider_unavailable,
};

/// Task history event that moves a task to `backlog` under a
/// [`ProviderFailureHold`].
pub const PROVIDER_FAILURE_HOLD_EVENT: &str = "provider_failure_hold";

/// The marker a hold's history note starts with, followed by the hold JSON.
pub const PROVIDER_FAILURE_HOLD_MARKER: &str = "[provider_failure_hold]";

/// The token naming the failing provider right after a provider marker in a
/// step failure: `[provider_refusal] provider=codex …`.
const PROVIDER_TOKEN: &str = "provider=";

/// The token naming the crew account that failed, right after the provider
/// token, when the failing crew sets `env` and so is its own account:
/// `[provider_limit] provider=claude account=opus-credits …`. A failure
/// without it, including every one written before accounts were named, is
/// the provider's default login's.
const ACCOUNT_TOKEN: &str = "account=";

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
    /// The provider account hit a usage limit [ORB-14695].
    Limit,
}

impl ProviderFailureClass {
    /// The class a step failure's code or message carries, if any: refusal,
    /// then usage limit, then capacity, then unavailability. A limit and
    /// capacity are checked before unavailability, which they are kinds of.
    #[must_use]
    pub fn of(error_code: Option<&str>, message: Option<&str>) -> Option<Self> {
        if is_provider_refusal(error_code, message) {
            Some(Self::Refusal)
        } else if is_provider_limit(error_code, message) {
            Some(Self::Limit)
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
            Self::Limit => PROVIDER_LIMIT_MARKER,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Capacity => "provider_capacity",
            Self::Unavailable => "provider_unavailable",
            Self::Refusal => "provider_refusal",
            Self::Limit => "provider_limit",
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

/// The crew account a step failure names right after its provider, if it
/// names one; `None` for the provider's default login.
#[must_use]
pub fn failed_account(message: &str) -> Option<&str> {
    let class = ProviderFailureClass::of(None, Some(message))?;
    let (_, rest) = message.split_once(class.marker())?;
    let mut tokens = rest.split_whitespace();
    tokens.next()?.strip_prefix(PROVIDER_TOKEN)?;
    let name = tokens.next()?.strip_prefix(ACCOUNT_TOKEN)?;
    let name = name.split(':').next()?;
    (!name.is_empty()).then_some(name)
}

/// `message` naming `account` after its provider token, so the hold and
/// exclusion it places stay on that crew account. A message that is not a
/// provider failure naming its provider, or already names an account, is
/// returned as it is.
#[must_use]
pub fn with_failed_account(message: &str, account: &str) -> String {
    let account = account.trim();
    if account.is_empty()
        || account.contains(char::is_whitespace)
        || failed_account(message).is_some()
        || failed_provider(message).is_none()
    {
        return message.to_string();
    }
    let Some(class) = ProviderFailureClass::of(None, Some(message)) else {
        return message.to_string();
    };
    let Some(marker_at) = message.find(class.marker()) else {
        return message.to_string();
    };
    let after_marker = marker_at + class.marker().len();
    let rest = &message[after_marker..];
    let token_start = after_marker + (rest.len() - rest.trim_start().len());
    let token_end = message[token_start..]
        .find(char::is_whitespace)
        .map_or(message.len(), |end| token_start + end);
    format!(
        "{} {ACCOUNT_TOKEN}{account}{}",
        &message[..token_end],
        &message[token_end..]
    )
}

/// `claude` for a provider's default login, `claude/opus-credits` for a crew
/// that is its own account: how every surface names a provider account.
#[must_use]
pub fn provider_account_label(provider: &str, account: Option<&str>) -> String {
    match account {
        Some(account) => format!("{provider}/{account}"),
        None => provider.to_string(),
    }
}

/// The tokens a usage-limit failure carries after its provider.
const LIMIT_MODEL_TOKEN: &str = "model=";
const LIMIT_WINDOW_TOKEN: &str = "window=";
const LIMIT_RESETS_AT_TOKEN: &str = "resets_at=";

/// What a provider said about the usage limit it hit [ORB-14695]: the model
/// the limit is scoped to, the limit's window, and when it resets. Each is
/// set only when the provider's own text said so; Orbit never invents one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderLimitFailure {
    /// The model or model family the limit covers (`opus`, `gpt-5-codex`),
    /// when the provider named one; otherwise the whole account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The limit's window as the provider labels it (`five_hour`,
    /// `seven_day_opus`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    /// When the provider said the limit resets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<DateTime<Utc>>,
}

impl ProviderLimitFailure {
    /// `[provider_limit] provider=<p> [model=<m>] [window=<w>]
    /// [resets_at=<rfc3339>] <detail>`: the step failure text of a limit.
    #[must_use]
    pub fn text(&self, provider: &str, detail: &str) -> String {
        let mut tokens = String::new();
        let single = |value: &str| value.split_whitespace().collect::<Vec<_>>().join("_");
        if let Some(model) = self
            .model
            .as_deref()
            .filter(|model| !model.trim().is_empty())
        {
            tokens.push_str(&format!("{LIMIT_MODEL_TOKEN}{} ", single(model)));
        }
        if let Some(window) = self
            .window
            .as_deref()
            .filter(|window| !window.trim().is_empty())
        {
            tokens.push_str(&format!("{LIMIT_WINDOW_TOKEN}{} ", single(window)));
        }
        if let Some(resets_at) = self.resets_at {
            tokens.push_str(&format!(
                "{LIMIT_RESETS_AT_TOKEN}{} ",
                resets_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            ));
        }
        provider_failure_text(
            ProviderFailureClass::Limit,
            provider,
            &format!("{tokens}{}", detail.trim()),
        )
    }

    /// The limit a step failure carries, if it is a usage-limit failure.
    #[must_use]
    pub fn from_text(message: &str) -> Option<Self> {
        if ProviderFailureClass::of(None, Some(message)) != Some(ProviderFailureClass::Limit) {
            return None;
        }
        let (_, rest) = message.split_once(PROVIDER_LIMIT_MARKER)?;
        let mut tokens = rest.split_whitespace();
        let mut limit = Self::default();
        if !tokens
            .next()
            .is_some_and(|token| token.starts_with(PROVIDER_TOKEN))
        {
            return Some(limit);
        }
        for token in tokens {
            if token.starts_with(ACCOUNT_TOKEN) {
                continue;
            } else if let Some(model) = token.strip_prefix(LIMIT_MODEL_TOKEN) {
                limit.model = Some(model.to_string());
            } else if let Some(window) = token.strip_prefix(LIMIT_WINDOW_TOKEN) {
                limit.window = Some(window.to_string());
            } else if let Some(resets_at) = token.strip_prefix(LIMIT_RESETS_AT_TOKEN) {
                limit.resets_at = DateTime::parse_from_rfc3339(resets_at)
                    .ok()
                    .map(|at| at.with_timezone(&Utc));
            } else {
                break;
            }
        }
        Some(limit)
    }
}

/// The crews a task may not run as until `not_before`, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderFailureHold {
    pub class: ProviderFailureClass,
    /// The provider that failed, when the run named it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The crew account that failed: a crew that sets `env` and so is its
    /// own account. `None` for the provider's default login, and for every
    /// hold stored before accounts were named, which reads as the
    /// provider's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// Crews admission does not draw for the task before `not_before`, on
    /// `machine` when the hold names one.
    pub excluded_crews: Vec<String>,
    /// [ORB-16379] The machine `excluded_crews` are excluded on: the host
    /// whose provider was unavailable. `None` excludes them on every machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// [ORB-16379] Crews a still-standing earlier hold excluded on every
    /// machine, which a machine-scoped hold keeps excluding everywhere.
    /// Empty unless `machine` is set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carried_crews: Vec<String>,
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
        let text = super::failure_classification_text(text);
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

    /// Whether `crew` is excluded by this hold on the machine it names, or
    /// on every machine when it names none.
    #[must_use]
    pub fn excludes(&self, crew: &str) -> bool {
        self.excluded_crews
            .iter()
            .chain(&self.carried_crews)
            .any(|excluded| excluded == crew)
    }

    /// [ORB-16379] Whether `excluded_crews` are excluded on `machine`. An
    /// executor of unknown identity is treated as the hold's machine.
    #[must_use]
    pub fn applies_on(&self, machine: Option<&str>) -> bool {
        match (self.machine.as_deref(), machine) {
            (Some(held), Some(machine)) => held == machine,
            _ => true,
        }
    }

    /// [ORB-16379] This hold as `machine` sees it: a hold that names no
    /// machine, excluding what it excludes there. `None` when it excludes
    /// nothing there because its crews were excluded on another machine.
    #[must_use]
    pub fn on_machine(&self, machine: Option<&str>) -> Option<Self> {
        let mut seen = self.clone();
        seen.machine = None;
        let carried = std::mem::take(&mut seen.carried_crews);
        if self.applies_on(machine) {
            seen.excluded_crews.extend(carried);
        } else if carried.is_empty() {
            return None;
        } else {
            seen.excluded_crews = carried;
        }
        seen.excluded_crews.sort();
        seen.excluded_crews.dedup();
        Some(seen)
    }

    /// [ORB-16379] Keep the crews a still-standing `previous` hold excluded,
    /// each in the scope it was excluded in: crews excluded on every machine
    /// stay excluded everywhere, and crews excluded on one machine stay
    /// scoped to it. When both holds are scoped to different machines this
    /// hold's machine wins; the other host's own outage record still keeps
    /// its drain off those crews. The hold lasts until the later
    /// `not_before`.
    pub fn absorb(&mut self, previous: Self) {
        let (scoped, mut everywhere) = self.scopes();
        let (previous_scoped, previous_everywhere) = previous.scopes();
        everywhere.extend(previous_everywhere);
        let scoped = match (scoped, previous_scoped) {
            (Some((machine, mut crews)), Some((previous_machine, previous_crews))) => {
                if machine == previous_machine {
                    crews.extend(previous_crews);
                }
                Some((machine, crews))
            }
            (Some(scoped), None) | (None, Some(scoped)) => Some(scoped),
            (None, None) => None,
        };
        let sorted = |mut crews: Vec<String>| {
            crews.sort();
            crews.dedup();
            crews
        };
        match scoped {
            Some((machine, crews)) => {
                self.machine = Some(machine);
                self.excluded_crews = sorted(crews);
                self.carried_crews = sorted(everywhere);
            }
            None => {
                self.machine = None;
                self.excluded_crews = sorted(everywhere);
                self.carried_crews = Vec::new();
            }
        }
        self.not_before = self.not_before.max(previous.not_before);
    }

    /// The crews excluded on one machine, with it, and those excluded on
    /// every machine.
    fn scopes(&self) -> (Option<(String, Vec<String>)>, Vec<String>) {
        match &self.machine {
            Some(machine) => (
                Some((machine.clone(), self.excluded_crews.clone())),
                self.carried_crews.clone(),
            ),
            None => (
                None,
                self.excluded_crews
                    .iter()
                    .chain(&self.carried_crews)
                    .cloned()
                    .collect(),
            ),
        }
    }

    /// `a, b` for an unscoped hold; `a on <machine>; b everywhere` for a
    /// scoped one.
    #[must_use]
    pub fn describe_crews(&self) -> String {
        let Some(machine) = &self.machine else {
            return self.excluded_crews.join(", ");
        };
        let scoped = format!("{} on {machine}", self.excluded_crews.join(", "));
        if self.carried_crews.is_empty() {
            scoped
        } else {
            format!("{scoped}; {} everywhere", self.carried_crews.join(", "))
        }
    }
}

/// [ORB-16379] A crew this host could not run because its provider was
/// unavailable here, say its login expired. Recorded in the host's store when
/// a local run fails `provider_unavailable`; the host's local admission draws
/// no task onto the crew until `until`, whatever became of the task that
/// failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCrewOutage {
    /// The provider that failed, when the run named it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub crew: String,
    /// When the crew may be drawn again: the failed task's hold's
    /// `not_before`.
    pub until: DateTime<Utc>,
    pub observed_at: DateTime<Utc>,
    /// The run whose failure recorded the outage.
    pub run_id: String,
}
