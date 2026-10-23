//! Read-time aliases for deprecated configuration spellings [ORB-16775].
//!
//! Consolidated key families keep loading from the spelling an existing
//! `config.toml` still carries. [`translate_deprecated_keys`] writes each
//! deprecated key's value into its replacement, in place, unless the same
//! document already states the replacement, which then wins. The deprecated
//! key stays in the document so the load warns about it by name
//! (`registry::DEPRECATED_CONFIG_KEYS`), and `orbit doctor
//! --fix-retired-config-keys` writes the translated replacement into the file
//! before deleting the old key.
//!
//! - `operation.review_crew` → `workflow.purpose_crews.review`;
//! - `workflow.final_recovery_crews` → `workflow.purpose_crews.final_recovery`;
//! - `workflow.purpose_crews.{ci_failure_sweep,dependabot,merged_head_repair}`
//!   → `workflow.purpose_crews.sweep_tasks`, one pool for every sweep-filed
//!   task. Old keys naming different pools in one file cannot become one
//!   pool, so that file is refused with the keys named;
//! - `review.before_pr` / `review.before_landing` (and a retired
//!   `operation.review_policy`) → `review.timing`;
//! - `machine.worker_containment_strict` → `machine.worker_containment =
//!   "strict"`; a boolean `machine.worker_containment` is read by the row's
//!   own resolver, so the file keeps its spelling and still warns;
//! - `workflow.validation_env.login_shell` / `.interactive` →
//!   `workflow.validation_env.shell_probe`.
//!
//! The paired switches layered one key at a time, so a workspace file could
//! state one half and inherit the other. Translating such a file needs the
//! layer beneath it: `lower` is that layer's document as read from disk, and
//! an unstated half takes its value there, as the old per-key merge did.
//!
//! A run captures the layer that decided its review timing, and an upgrade
//! resume compares that capture with today's policy. So a load leaves
//! `review.timing` unwritten when the half that decides it is inherited and
//! the layer beneath already resolves to the same timing: the decision keeps
//! the layer it had. The doctor rewrite writes the timing explicitly instead,
//! so the file no longer depends on a key it is about to lose.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;

use crate::layering::{set_value_at_path, value_at_path};
use crate::operation::{
    LEGACY_REVIEW_BEFORE_LANDING_KEY, LEGACY_REVIEW_BEFORE_PR_KEY, LEGACY_REVIEW_CREW_KEY,
    REVIEW_CREW_KEY, REVIEW_TIMING_KEY, ReviewTimingSetting, legacy_policy_before_pr,
    translate_legacy_review_minutes,
};
use crate::registry::read_optional;

/// The consolidated final-recovery pool.
pub(crate) const FINAL_RECOVERY_CREWS_KEY: &str = "workflow.purpose_crews.final_recovery";
/// The deprecated spelling of [`FINAL_RECOVERY_CREWS_KEY`].
pub(crate) const LEGACY_FINAL_RECOVERY_CREWS_KEY: &str = "workflow.final_recovery_crews";
/// The one pool every sweep-filed task draws from.
pub(crate) const SWEEP_TASKS_CREWS_KEY: &str = "workflow.purpose_crews.sweep_tasks";
/// The per-sweep pools [`SWEEP_TASKS_CREWS_KEY`] replaced.
pub(crate) const LEGACY_SWEEP_CREWS_KEYS: &[&str] = &[
    "workflow.purpose_crews.ci_failure_sweep",
    "workflow.purpose_crews.dependabot",
    "workflow.purpose_crews.merged_head_repair",
];
/// Worker containment mode: `off`, `warn` or `strict`.
pub(crate) const WORKER_CONTAINMENT_KEY: &str = "machine.worker_containment";
/// The deprecated strict switch, translated to `worker_containment = "strict"`.
pub(crate) const LEGACY_WORKER_CONTAINMENT_STRICT_KEY: &str = "machine.worker_containment_strict";
/// Validation-environment shell probe: `off`, `login` or `interactive`.
pub(crate) const SHELL_PROBE_KEY: &str = "workflow.validation_env.shell_probe";
/// The deprecated probe switch: `false` never probes.
pub(crate) const LEGACY_LOGIN_SHELL_KEY: &str = "workflow.validation_env.login_shell";
/// The deprecated interactive switch: `false` probes only `-l -c`.
pub(crate) const LEGACY_INTERACTIVE_KEY: &str = "workflow.validation_env.interactive";

/// Every replacement key a translation may write.
const REPLACEMENT_KEYS: &[&str] = &[
    REVIEW_CREW_KEY,
    FINAL_RECOVERY_CREWS_KEY,
    SWEEP_TASKS_CREWS_KEY,
    REVIEW_TIMING_KEY,
    crate::operation::REVIEW_MINUTES_KEY,
    WORKER_CONTAINMENT_KEY,
    SHELL_PROBE_KEY,
];

/// Why a document is translated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Purpose {
    /// Reading config: keep each decision's layer where one key can.
    Load,
    /// Rewriting the file: state every replacement the file relies on.
    Rewrite,
}

/// Translate `document`'s deprecated spellings into their replacements, in
/// place, before anything reads it. `lower` is the layer beneath `document`
/// as read from disk (the global file under a workspace file), or `None` for
/// the lowest file, whose unstated halves take their built-in defaults.
pub fn translate_deprecated_keys(
    document: &mut toml::Value,
    lower: Option<&toml::Value>,
    config_path: &Path,
) -> Result<(), OrbitError> {
    translate(document, lower, config_path, Purpose::Load)
}

fn translate(
    document: &mut toml::Value,
    lower: Option<&toml::Value>,
    config_path: &Path,
    purpose: Purpose,
) -> Result<(), OrbitError> {
    translate_legacy_review_minutes(document, config_path)?;
    rename(document, LEGACY_REVIEW_CREW_KEY, REVIEW_CREW_KEY);
    rename(
        document,
        LEGACY_FINAL_RECOVERY_CREWS_KEY,
        FINAL_RECOVERY_CREWS_KEY,
    );
    translate_sweep_pools(document, config_path)?;
    translate_review_timing(document, lower, config_path, purpose)?;
    translate_worker_containment(document, config_path)?;
    translate_shell_probe(document, lower, config_path)?;
    Ok(())
}

/// The replacement values a file needs written so it says what it means
/// without its deprecated spellings: each replacement key the translated
/// file states that `original` (the file as written) does not, or states
/// only in a deprecated value form. `lower` is the layer beneath, as for
/// [`translate_deprecated_keys`]. `orbit doctor --fix-retired-config-keys`
/// writes these, then deletes the deprecated keys.
pub fn deprecated_key_rewrites(
    original: &toml::Value,
    lower: Option<&toml::Value>,
    config_path: &Path,
) -> Result<Vec<(&'static str, toml::Value)>, OrbitError> {
    let mut translated = original.clone();
    translate(&mut translated, lower, config_path, Purpose::Rewrite)?;
    Ok(REPLACEMENT_KEYS
        .iter()
        .filter_map(|key| {
            let value = value_at_path(&translated, key)?;
            let value = match value {
                toml::Value::Boolean(contained) if *key == WORKER_CONTAINMENT_KEY => {
                    let mode = if *contained {
                        WorkerContainmentMode::Warn
                    } else {
                        WorkerContainmentMode::Off
                    };
                    toml::Value::String(mode.as_str().to_string())
                }
                value => value.clone(),
            };
            match value_at_path(original, key) {
                Some(written) if *written == value => None,
                Some(toml::Value::Boolean(_)) if *key == WORKER_CONTAINMENT_KEY => {
                    Some((*key, value))
                }
                Some(_) => None,
                None => Some((*key, value)),
            }
        })
        .collect())
}

fn rename(document: &mut toml::Value, legacy: &str, replacement: &str) {
    if value_at_path(document, replacement).is_some() {
        return;
    }
    if let Some(value) = value_at_path(document, legacy).cloned() {
        set_value_at_path(document, replacement, value);
    }
}

fn translate_sweep_pools(document: &mut toml::Value, config_path: &Path) -> Result<(), OrbitError> {
    if value_at_path(document, SWEEP_TASKS_CREWS_KEY).is_some() {
        return Ok(());
    }
    let stated = LEGACY_SWEEP_CREWS_KEYS
        .iter()
        .filter_map(|key| value_at_path(document, key).map(|value| (*key, value.clone())))
        .collect::<Vec<_>>();
    let Some((_, first)) = stated.first() else {
        return Ok(());
    };
    if stated.iter().any(|(_, value)| value != first) {
        let keys = stated
            .iter()
            .map(|(key, _)| *key)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(OrbitError::InvalidInput(format!(
            "{}: {keys} (deprecated) name different pools, and every sweep-filed task now draws \
             from one: set {SWEEP_TASKS_CREWS_KEY} and delete them",
            display_path(config_path)
        )));
    }
    let first = first.clone();
    set_value_at_path(document, SWEEP_TASKS_CREWS_KEY, first);
    Ok(())
}

/// The before-landing halves a document states: `review.timing` decides both
/// when present; otherwise each deprecated switch (or a retired policy, for
/// the before-PR half) decides its own.
fn review_halves(
    document: &toml::Value,
    config_path: &Path,
) -> Result<(Option<bool>, Option<bool>), OrbitError> {
    if let Some(raw) = read_optional::<String>(document, REVIEW_TIMING_KEY, config_path)? {
        let (before_pr, before_landing) =
            ReviewTimingSetting::parse_in_file(&raw, config_path)?.switches();
        return Ok((Some(before_pr), Some(before_landing)));
    }
    let before_pr = match read_optional::<bool>(document, LEGACY_REVIEW_BEFORE_PR_KEY, config_path)?
    {
        Some(before_pr) => Some(before_pr),
        None => legacy_policy_before_pr(document, config_path)?,
    };
    let before_landing =
        read_optional::<bool>(document, LEGACY_REVIEW_BEFORE_LANDING_KEY, config_path)?;
    Ok((before_pr, before_landing))
}

fn translate_review_timing(
    document: &mut toml::Value,
    lower: Option<&toml::Value>,
    config_path: &Path,
    purpose: Purpose,
) -> Result<(), OrbitError> {
    if value_at_path(document, REVIEW_TIMING_KEY).is_some() {
        return Ok(());
    }
    let (stated_before_pr, stated_before_landing) = review_halves(document, config_path)?;
    if stated_before_pr.is_none() && stated_before_landing.is_none() {
        return Ok(());
    }
    let (lower_before_pr, lower_before_landing) = match lower {
        Some(lower) => review_halves(lower, config_path)?,
        None => (None, None),
    };
    let lower_before_pr = lower_before_pr.unwrap_or(false);
    let lower_before_landing = lower_before_landing.unwrap_or(false);
    let before_pr = stated_before_pr.unwrap_or(lower_before_pr);
    let before_landing = stated_before_landing.unwrap_or(lower_before_landing);
    let timing =
        ReviewTimingSetting::from_switches(before_pr, before_landing).ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "{}: {LEGACY_REVIEW_BEFORE_PR_KEY} and {LEGACY_REVIEW_BEFORE_LANDING_KEY} \
                 (deprecated) would both be on, counting a half this file leaves to the global \
                 file; there is one review layer before landing, so set {REVIEW_TIMING_KEY} to \
                 one of them",
                display_path(config_path)
            ))
        })?;
    // The old policy took its timing's layer from the half that decided it:
    // `before_landing` for a before-landing timing, `before_pr` otherwise.
    let decided_here = if timing == ReviewTimingSetting::BeforeLanding {
        stated_before_landing.is_some()
    } else {
        stated_before_pr.is_some()
    };
    let inherited =
        ReviewTimingSetting::from_switches(lower_before_pr, lower_before_landing) == Some(timing);
    if purpose == Purpose::Load && !decided_here && inherited {
        return Ok(());
    }
    set_value_at_path(
        document,
        REVIEW_TIMING_KEY,
        toml::Value::String(timing.as_str().to_string()),
    );
    Ok(())
}

fn translate_worker_containment(
    document: &mut toml::Value,
    config_path: &Path,
) -> Result<(), OrbitError> {
    let Some(strict) =
        read_optional::<bool>(document, LEGACY_WORKER_CONTAINMENT_STRICT_KEY, config_path)?
    else {
        return Ok(());
    };
    let contained = match value_at_path(document, WORKER_CONTAINMENT_KEY) {
        // A mode written as the new string wins over the deprecated switch.
        Some(toml::Value::String(_)) => return Ok(()),
        Some(toml::Value::Boolean(contained)) => *contained,
        Some(_) => {
            return Err(OrbitError::InvalidInput(format!(
                "{WORKER_CONTAINMENT_KEY} must be one of off, warn, strict"
            )));
        }
        None => true,
    };
    let mode = match (contained, strict) {
        (false, true) => {
            return Err(OrbitError::InvalidInput(format!(
                "{}: {LEGACY_WORKER_CONTAINMENT_STRICT_KEY}=true requires \
                 {WORKER_CONTAINMENT_KEY}=true; set {WORKER_CONTAINMENT_KEY} = \"strict\" instead",
                display_path(config_path)
            )));
        }
        (true, true) => "strict",
        (true, false) => "warn",
        (false, false) => "off",
    };
    set_value_at_path(
        document,
        WORKER_CONTAINMENT_KEY,
        toml::Value::String(mode.to_string()),
    );
    Ok(())
}

/// The probe halves a document states: `shell_probe` decides both when
/// present (`off` leaves the interactive half at its default); otherwise each
/// deprecated switch decides its own.
fn probe_halves(
    document: &toml::Value,
    config_path: &Path,
) -> Result<(Option<bool>, Option<bool>), OrbitError> {
    if let Some(raw) = read_optional::<String>(document, SHELL_PROBE_KEY, config_path)? {
        return Ok(match ShellProbeMode::parse(&raw)? {
            ShellProbeMode::Off => (Some(false), None),
            ShellProbeMode::Login => (Some(true), Some(false)),
            ShellProbeMode::Interactive => (Some(true), Some(true)),
        });
    }
    Ok((
        read_optional::<bool>(document, LEGACY_LOGIN_SHELL_KEY, config_path)?,
        read_optional::<bool>(document, LEGACY_INTERACTIVE_KEY, config_path)?,
    ))
}

fn translate_shell_probe(
    document: &mut toml::Value,
    lower: Option<&toml::Value>,
    config_path: &Path,
) -> Result<(), OrbitError> {
    if value_at_path(document, SHELL_PROBE_KEY).is_some() {
        return Ok(());
    }
    let (login_shell, interactive) = probe_halves(document, config_path)?;
    if login_shell.is_none() && interactive.is_none() {
        return Ok(());
    }
    let (lower_login_shell, lower_interactive) = match lower {
        Some(lower) => probe_halves(lower, config_path)?,
        None => (None, None),
    };
    let mode = match (
        login_shell.or(lower_login_shell).unwrap_or(true),
        interactive.or(lower_interactive).unwrap_or(true),
    ) {
        (false, _) => ShellProbeMode::Off,
        (true, false) => ShellProbeMode::Login,
        (true, true) => ShellProbeMode::Interactive,
    };
    set_value_at_path(
        document,
        SHELL_PROBE_KEY,
        toml::Value::String(mode.as_str().to_string()),
    );
    Ok(())
}

/// `workflow.validation_env.shell_probe`: how required validation resolves
/// PATH and toolchain locators from the owner's shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellProbeMode {
    /// Never probe the shell.
    Off,
    /// Probe a login shell (`-l -c`) only.
    Login,
    /// Try an interactive login shell (`-i -l -c`), falling back to `-l -c`
    /// (the default).
    Interactive,
}

impl ShellProbeMode {
    /// Every value `shell_probe` admits.
    pub const CHOICES: &'static [&'static str] = &["off", "login", "interactive"];

    /// The configured spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Login => "login",
            Self::Interactive => "interactive",
        }
    }

    /// The mode a configured spelling names.
    pub fn parse(raw: &str) -> Result<Self, OrbitError> {
        match raw.trim() {
            "off" => Ok(Self::Off),
            "login" => Ok(Self::Login),
            "interactive" => Ok(Self::Interactive),
            other => Err(OrbitError::InvalidInput(format!(
                "{SHELL_PROBE_KEY} has invalid value '{other}'; expected one of: {}",
                Self::CHOICES.join(", ")
            ))),
        }
    }

    /// Whether the shell is probed at all.
    pub fn probes(self) -> bool {
        self != Self::Off
    }

    /// Whether the interactive login shell is tried first.
    pub fn interactive(self) -> bool {
        self == Self::Interactive
    }
}

/// `machine.worker_containment`: how detached pipeline workers are bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerContainmentMode {
    /// Launch workers in the caller's cgroup.
    Off,
    /// Launch each worker in its own scope, or in the caller's cgroup with a
    /// warning when no user manager is reachable (the default).
    Warn,
    /// Refuse a launch that cannot get its own scope.
    Strict,
}

impl WorkerContainmentMode {
    /// Every value `worker_containment` admits.
    pub const CHOICES: &'static [&'static str] = &["off", "warn", "strict"];

    /// The configured spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Warn => "warn",
            Self::Strict => "strict",
        }
    }

    /// The mode a configured spelling names.
    pub fn parse(raw: &str) -> Result<Self, OrbitError> {
        match raw.trim() {
            "off" => Ok(Self::Off),
            "warn" => Ok(Self::Warn),
            "strict" => Ok(Self::Strict),
            other => Err(OrbitError::InvalidInput(format!(
                "{WORKER_CONTAINMENT_KEY} has invalid value '{other}'; expected one of: {}",
                Self::CHOICES.join(", ")
            ))),
        }
    }
}

/// `machine.worker_containment` as written: a mode, or the deprecated
/// boolean (`true` is `warn`, `false` is `off`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(untagged)]
pub(crate) enum WorkerContainmentSetting {
    Mode(String),
    Legacy(bool),
}

pub(crate) fn resolve_worker_containment(
    raw: Option<WorkerContainmentSetting>,
) -> Result<WorkerContainmentMode, OrbitError> {
    match raw {
        None | Some(WorkerContainmentSetting::Legacy(true)) => Ok(WorkerContainmentMode::Warn),
        Some(WorkerContainmentSetting::Legacy(false)) => Ok(WorkerContainmentMode::Off),
        Some(WorkerContainmentSetting::Mode(mode)) => WorkerContainmentMode::parse(&mode),
    }
}

/// Whether `document` writes `machine.worker_containment` as the deprecated
/// boolean.
pub(crate) fn has_legacy_worker_containment(document: &toml::Value) -> bool {
    matches!(
        value_at_path(document, WORKER_CONTAINMENT_KEY),
        Some(toml::Value::Boolean(_))
    )
}

fn display_path(config_path: &Path) -> String {
    redact_home_dir(&config_path.display().to_string())
}
