//! Typed review preferences [ORB-11333] [ORB-13992].
//!
//! Each review mechanism has one switch. Before-PR review is
//! `review.before_pr` (default off), bounded by `review.minutes` per
//! candidate; `operation.review_crew` names the crew of automatic review.
//! After-landing review has no key here at all: its only switch is the
//! `delivery-code-review` auto-task's own `enabled` flag, which Core reads.
//!
//! Each layer states any subset; a value resolves built-in → global →
//! workspace with its winning layer recorded.
//!
//! `[[review.host_evidence]]` declares the checks a claimed leaf's host owes
//! for the paths its candidate changed ([`HostEvidenceRule`]); a layer that
//! states the list replaces every rule an earlier layer stated.
//!
//! The retired `operation.review_policy` enum and `operation.review_minutes`
//! are translated when a document is parsed ([`translate_legacy_review_keys`])
//! and warned as deprecated (`registry::DEPRECATED_CONFIG_KEYS`): a legacy
//! policy states `review.before_pr` for its layer, `after-landing` also
//! enables the auto-task until its flag is set explicitly, and the legacy
//! minutes become `review.minutes`. A key the same document states in
//! `[review]` wins over its legacy spelling. The retired budget keys
//! (`operation.review_reviewer_starts`, `operation.review_repair_cycles`) are
//! warned and ignored (`registry::REMOVED_CONFIG_KEYS`).

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::policy::compile_glob_regex;
use orbit_types::task::validate_relative_artifact_path;
use orbit_types::workflow::{
    DEFAULT_REVIEW_MINUTES, HostEvidenceRule, HostSandboxCommand, ReviewBudget, ReviewEvidenceKind,
};
use serde::{Deserialize, Serialize};

use crate::layering::{set_value_at_path, value_at_path};
use crate::registry::{deprecated_key_note, read_optional, removed_key_note};

/// Version of the resolved-policy shape captured into run records. Bump when
/// a captured field is added, removed, or changes meaning so an older
/// snapshot fails closed for privileged actions instead of being
/// reinterpreted. Version 2 added the independent review budget fields
/// [ORB-11333]; version 3 replaced the review policy enum with
/// `review.before_pr` and dropped the reviewer-start budget [ORB-13992];
/// version 4 added `review.host_evidence`.
pub const OPERATION_POLICY_VERSION: u32 = 4;

const MAX_REVIEW_MINUTES: u32 = 1_440;

/// The before-PR switch.
pub const REVIEW_BEFORE_PR_KEY: &str = "review.before_pr";
/// The before-PR review's time limit.
pub const REVIEW_MINUTES_KEY: &str = "review.minutes";
/// The retired review timing enum, translated on load.
pub const LEGACY_REVIEW_POLICY_KEY: &str = "operation.review_policy";
/// The retired lineage minutes, translated to [`REVIEW_MINUTES_KEY`].
pub const LEGACY_REVIEW_MINUTES_KEY: &str = "operation.review_minutes";

/// Commands settlement may rerun to confirm a red-base claim [ORB-14434].
/// Not review policy: [`crate::ConfigSnapshot`] reads it, not
/// [`OperationLayer`].
const REVIEW_BASELINE_COMMANDS_KEY: &str = "review.baseline_commands";

/// Checks a claimed leaf's host owes for the paths it changed.
pub const REVIEW_HOST_EVIDENCE_KEY: &str = "review.host_evidence";

/// Every live `[review]` key, as the unknown-key guard sees it.
const REVIEW_KEYS: &[&str] = &[
    REVIEW_BEFORE_PR_KEY,
    REVIEW_MINUTES_KEY,
    REVIEW_BASELINE_COMMANDS_KEY,
    REVIEW_HOST_EVIDENCE_KEY,
];

/// Every live `[operation]` key, as the unknown-key guard sees it.
const OPERATION_KEYS: &[&str] = &["operation.review_crew"];

/// A retired `operation.review_policy` value, read only to translate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegacyReviewPolicy {
    None,
    BeforePr,
    AfterLanding,
}

impl LegacyReviewPolicy {
    const CHOICES: &'static [&'static str] = &["none", "before-pr", "after-landing"];

    fn parse(raw: &str) -> Result<Self, OrbitError> {
        match raw.trim() {
            "none" => Ok(Self::None),
            "before-pr" => Ok(Self::BeforePr),
            "after-landing" => Ok(Self::AfterLanding),
            other => Err(OrbitError::InvalidInput(format!(
                "{LEGACY_REVIEW_POLICY_KEY} (deprecated) has invalid value '{other}'; expected \
                 one of: {}",
                Self::CHOICES.join(", ")
            ))),
        }
    }

    fn read(document: &toml::Value, config_path: &Path) -> Result<Option<Self>, OrbitError> {
        read_optional::<String>(document, LEGACY_REVIEW_POLICY_KEY, config_path)?
            .map(|raw| Self::parse(&raw))
            .transpose()
    }
}

/// Translate one document's deprecated review keys into their `[review]`
/// spelling, in place, before anything reads it [ORB-13992]. A key the
/// document already states under `[review]` is left as written. The legacy
/// keys stay in the document so the load warns about them by name.
pub(crate) fn translate_legacy_review_keys(
    document: &mut toml::Value,
    config_path: &Path,
) -> Result<(), OrbitError> {
    if let Some(policy) = LegacyReviewPolicy::read(document, config_path)?
        && value_at_path(document, REVIEW_BEFORE_PR_KEY).is_none()
    {
        set_value_at_path(
            document,
            REVIEW_BEFORE_PR_KEY,
            toml::Value::Boolean(policy == LegacyReviewPolicy::BeforePr),
        );
    }
    let minutes = bounded(
        read_optional::<u32>(document, LEGACY_REVIEW_MINUTES_KEY, config_path)?,
        LEGACY_REVIEW_MINUTES_KEY,
        1,
        MAX_REVIEW_MINUTES,
    )?;
    if let Some(minutes) = minutes
        && value_at_path(document, REVIEW_MINUTES_KEY).is_none()
    {
        set_value_at_path(
            document,
            REVIEW_MINUTES_KEY,
            toml::Value::Integer(i64::from(minutes)),
        );
    }
    Ok(())
}

/// Which layer supplied a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OperationLayerSource {
    /// The crate's built-in defaults.
    BuiltIn,
    /// The global `config.toml`.
    Global,
    /// The workspace `config.toml`.
    Workspace,
}

impl OperationLayerSource {
    /// The label captured into review admissions and shown in diagnostics.
    pub fn label(self) -> &'static str {
        match self {
            OperationLayerSource::BuiltIn => "built-in",
            OperationLayerSource::Global => "global",
            OperationLayerSource::Workspace => "workspace",
        }
    }
}

/// One resolved field with its provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationField<T> {
    /// The resolved value.
    pub value: T,
    /// The layer that decided it.
    pub source: OperationLayerSource,
}

impl<T: Clone> OperationField<T> {
    fn built_in(value: T) -> Self {
        Self {
            value,
            source: OperationLayerSource::BuiltIn,
        }
    }

    fn set(&mut self, value: Option<&T>, layer: OperationLayerSource) {
        if let Some(value) = value {
            self.value = value.clone();
            self.source = layer;
        }
    }
}

/// The explicit review statements one layer makes. Every field is optional:
/// an omitted field inherits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OperationLayer {
    /// Explicit `review.before_pr`, or its translation from a legacy policy.
    pub review_before_pr: Option<bool>,
    /// Explicit `review.minutes`, or the legacy `operation.review_minutes`.
    pub review_minutes: Option<u32>,
    /// Explicit review crew.
    pub review_crew: Option<String>,
    /// Explicit `[[review.host_evidence]]` rules; an empty list clears the
    /// rules an earlier layer stated.
    pub review_host_evidence: Option<Vec<HostEvidenceRule>>,
    /// What a legacy `operation.review_policy` says about after-landing
    /// review: `Some(true)` for `after-landing`, `Some(false)` for any other
    /// value, `None` when the layer does not state the legacy key.
    pub legacy_after_landing: Option<bool>,
}

impl OperationLayer {
    /// Read the review keys of one translated document. Unknown keys and
    /// out-of-range values fail rather than being ignored: a misspelled
    /// review setting that silently resolved to off would be a surprising
    /// statement. Deprecated and removed `[operation]` keys are the
    /// exception: the loader warns about them by name, so a `config.toml`
    /// written before their retirement keeps loading.
    pub fn from_document(document: &toml::Value, config_path: &Path) -> Result<Self, OrbitError> {
        reject_unknown_keys(document, "operation", OPERATION_KEYS, true)?;
        reject_unknown_keys(document, "review", REVIEW_KEYS, false)?;

        Ok(Self {
            review_before_pr: read_optional(document, REVIEW_BEFORE_PR_KEY, config_path)?,
            review_minutes: review_minutes(read_optional(
                document,
                REVIEW_MINUTES_KEY,
                config_path,
            )?)?,
            review_crew: review_crew(read_optional(
                document,
                "operation.review_crew",
                config_path,
            )?)?,
            review_host_evidence: read_optional::<Vec<HostEvidenceRule>>(
                document,
                REVIEW_HOST_EVIDENCE_KEY,
                config_path,
            )?
            .map(host_evidence_rules)
            .transpose()?,
            legacy_after_landing: LegacyReviewPolicy::read(document, config_path)?
                .map(|policy| policy == LegacyReviewPolicy::AfterLanding),
        })
    }

    /// Whether this layer states anything at all.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Refuse a key of `[table]` that is not live (`known`) and, when
/// `retired_allowed`, not a deprecated or removed key either.
fn reject_unknown_keys(
    document: &toml::Value,
    table: &str,
    known: &[&str],
    retired_allowed: bool,
) -> Result<(), OrbitError> {
    let Some(value) = document.as_table().and_then(|root| root.get(table)) else {
        return Ok(());
    };
    let entries = value
        .as_table()
        .ok_or_else(|| OrbitError::InvalidInput(format!("[{table}] must be a table")))?;
    for key in entries.keys() {
        let qualified = format!("{table}.{key}");
        let retired = retired_allowed
            && (deprecated_key_note(&qualified).is_some()
                || removed_key_note(&qualified).is_some());
        if !known.contains(&qualified.as_str()) && !retired {
            return Err(OrbitError::InvalidInput(format!(
                "[{table}] has unknown key '{key}'; expected one of: {}",
                known
                    .iter()
                    .map(|known| known.trim_start_matches(table).trim_start_matches('.'))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }
    Ok(())
}

/// The fully resolved review preferences with per-field provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationPolicy {
    /// [`OPERATION_POLICY_VERSION`] at resolution time.
    pub version: u32,
    /// Whether a delivery submitted now holds PR creation for a reviewer.
    pub review_before_pr: OperationField<bool>,
    /// Reviewer runtime minutes for one candidate's before-PR review.
    pub review_minutes: OperationField<u32>,
    /// Crew selected for automatic review: the before-PR reviewer and the
    /// crew of minted after-landing review tasks.
    pub review_crew: OperationField<Option<String>>,
    /// Checks a claimed leaf's host owes for the paths it changed.
    #[serde(default = "no_host_evidence")]
    pub review_host_evidence: OperationField<Vec<HostEvidenceRule>>,
    /// The layer whose legacy `operation.review_policy = after-landing`
    /// still enables the `delivery-code-review` auto-task while that
    /// definition's own flag was never set explicitly. `None` when no
    /// legacy statement asks for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_after_landing: Option<OperationLayerSource>,
}

impl Default for OperationPolicy {
    fn default() -> Self {
        Self::built_in()
    }
}

impl OperationPolicy {
    /// The built-in layer: no before-PR review, the default time limit.
    pub fn built_in() -> Self {
        Self {
            version: OPERATION_POLICY_VERSION,
            review_before_pr: OperationField::built_in(false),
            review_minutes: OperationField::built_in(DEFAULT_REVIEW_MINUTES),
            review_crew: OperationField::built_in(None),
            review_host_evidence: no_host_evidence(),
            legacy_after_landing: None,
        }
    }

    /// Resolve the layers in precedence order over the built-in defaults.
    pub fn resolve(layers: &[(OperationLayerSource, &OperationLayer)]) -> Self {
        let mut policy = Self::built_in();
        for (source, layer) in layers {
            policy.apply_layer(*source, layer);
        }
        policy
    }

    fn apply_layer(&mut self, source: OperationLayerSource, layer: &OperationLayer) {
        self.review_before_pr
            .set(layer.review_before_pr.as_ref(), source);
        self.review_minutes
            .set(layer.review_minutes.as_ref(), source);
        if let Some(crew) = &layer.review_crew {
            self.review_crew = OperationField {
                value: Some(crew.clone()),
                source,
            };
        }
        self.review_host_evidence
            .set(layer.review_host_evidence.as_ref(), source);
        if let Some(after_landing) = layer.legacy_after_landing {
            self.legacy_after_landing = after_landing.then_some(source);
        }
    }

    /// The review limit the gate captures at admission.
    pub fn review_budget(&self) -> ReviewBudget {
        ReviewBudget {
            minutes: self.review_minutes.value,
        }
    }
}

pub(crate) fn review_minutes(raw: Option<u32>) -> Result<Option<u32>, OrbitError> {
    bounded(raw, REVIEW_MINUTES_KEY, 1, MAX_REVIEW_MINUTES)
}

fn no_host_evidence() -> OperationField<Vec<HostEvidenceRule>> {
    OperationField::built_in(Vec::new())
}

/// Refuse a rule Orbit could never fulfil, so a misconfiguration fails at
/// load instead of holding every matching review forever.
fn host_evidence_rules(rules: Vec<HostEvidenceRule>) -> Result<Vec<HostEvidenceRule>, OrbitError> {
    let invalid = |index: usize, why: String| {
        OrbitError::InvalidInput(format!("{REVIEW_HOST_EVIDENCE_KEY}[{index}] {why}"))
    };
    let mut artifacts = std::collections::BTreeSet::new();
    rules
        .into_iter()
        .enumerate()
        .map(|(index, rule)| {
            let rule = HostEvidenceRule {
                name: rule.name.trim().to_string(),
                command: rule.command.trim().to_string(),
                artifact: rule.artifact.trim().to_string(),
                paths: rule
                    .paths
                    .iter()
                    .map(|path| path.trim().to_string())
                    .collect(),
                ..rule
            };
            if !HostEvidenceRule::admits_kind(rule.kind) {
                return Err(invalid(
                    index,
                    "has kind outside codeql and host_sandbox_test".to_string(),
                ));
            }
            if rule.name.is_empty() || rule.command.is_empty() {
                return Err(invalid(index, "needs a name and a command".to_string()));
            }
            if rule.paths.is_empty() {
                return Err(invalid(index, "needs at least one path glob".to_string()));
            }
            for pattern in &rule.paths {
                compile_glob_regex(pattern).map_err(|error| {
                    invalid(index, format!("has invalid path glob `{pattern}`: {error}"))
                })?;
            }
            validate_relative_artifact_path(&rule.artifact)
                .map_err(|error| invalid(index, format!("has an invalid artifact: {error}")))?;
            if !rule.artifact.ends_with(".json") || rule.artifact.starts_with("./") {
                return Err(invalid(
                    index,
                    format!("artifact `{}` must be a relative .json path", rule.artifact),
                ));
            }
            if !artifacts.insert(rule.artifact.clone()) {
                return Err(invalid(
                    index,
                    format!("repeats artifact `{}`", rule.artifact),
                ));
            }
            if rule.kind == ReviewEvidenceKind::HostSandboxTest {
                HostSandboxCommand::admit(&rule.command, &[])
                    .map_err(|refusal| invalid(index, format!("command: {}", refusal.detail)))?;
            }
            Ok(rule)
        })
        .collect()
}

pub(crate) fn review_crew(raw: Option<String>) -> Result<Option<String>, OrbitError> {
    match raw {
        Some(value) if value.trim().is_empty() => Err(OrbitError::InvalidInput(
            "operation.review_crew must not be empty".to_string(),
        )),
        Some(value) => Ok(Some(value.trim().to_string())),
        None => Ok(None),
    }
}

fn bounded<T>(raw: Option<T>, key: &str, min: T, max: T) -> Result<Option<T>, OrbitError>
where
    T: PartialOrd + std::fmt::Display + Copy,
{
    match raw {
        Some(value) if value < min || value > max => Err(OrbitError::InvalidInput(format!(
            "{key} has invalid value {value}; expected {min}..={max}"
        ))),
        other => Ok(other),
    }
}
