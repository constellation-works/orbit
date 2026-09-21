//! Typed `[operation]` review preferences.
//!
//! The table once carried operation-mode presets and grants; those were
//! removed on 2026-09-21 (see `docs/design/orbit-core/4_decisions.md`). What
//! remains is the automatic review policy [ORB-11333] under the unchanged
//! `[operation]` table name: `review_policy`, `review_crew`, and the lineage
//! budget fields. Each layer states any subset; a value resolves built-in →
//! global → workspace with its winning layer recorded, and removed keys are
//! warned and ignored at load (`registry::REMOVED_CONFIG_KEYS`) so an
//! existing `config.toml` keeps loading.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::{ReviewBudget, ReviewTiming};
use serde::{Deserialize, Serialize};

use crate::registry::{read_optional, removed_key_note};

/// Version of the resolved-policy shape captured into run records. Bump when
/// a captured field is added, removed, or changes meaning so an older
/// snapshot fails closed for privileged actions instead of being
/// reinterpreted. Version 2 added the independent review budget fields
/// [ORB-11333]; retiring the never-captured preset fields left it unchanged.
pub const OPERATION_POLICY_VERSION: u32 = 2;

const MAX_REVIEW_REVIEWER_STARTS: u32 = 10;
const MAX_REVIEW_REPAIR_CYCLES: u32 = 10;
const MAX_REVIEW_MINUTES: u32 = 1_440;

/// When automatic code review applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewPolicy {
    /// No automatic review managed by this policy.
    #[serde(rename = "none")]
    None,
    /// Hold PR creation for a fresh reviewer with scoped repairs [ORB-11333].
    #[serde(rename = "before-pr")]
    BeforePr,
    /// Accumulate uncovered landed deliveries for a scheduled review.
    #[serde(rename = "after-landing")]
    AfterLanding,
}

impl ReviewPolicy {
    /// The configuration key this choice is read from.
    pub const KEY: &'static str = "operation.review_policy";
    /// Every accepted literal, in declaration order.
    pub const CHOICES: &'static [&'static str] = &["none", "before-pr", "after-landing"];

    /// The literal spelling written in `config.toml`.
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewPolicy::None => "none",
            ReviewPolicy::BeforePr => "before-pr",
            ReviewPolicy::AfterLanding => "after-landing",
        }
    }

    /// Parse a literal, naming the key and the accepted values on failure.
    pub fn parse(raw: &str) -> Result<Self, OrbitError> {
        match raw.trim() {
            "none" => Ok(ReviewPolicy::None),
            "before-pr" => Ok(ReviewPolicy::BeforePr),
            "after-landing" => Ok(ReviewPolicy::AfterLanding),
            other => Err(OrbitError::InvalidInput(format!(
                "{} has invalid value '{other}'; expected one of: {}",
                Self::KEY,
                Self::CHOICES.join(", ")
            ))),
        }
    }

    /// The shared review-timing contract this preference selects.
    pub fn timing(self) -> ReviewTiming {
        match self {
            ReviewPolicy::None => ReviewTiming::None,
            ReviewPolicy::BeforePr => ReviewTiming::BeforePr,
            ReviewPolicy::AfterLanding => ReviewTiming::AfterLanding,
        }
    }
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

/// The explicit `[operation]` statements one layer makes. Every field is
/// optional: an omitted field inherits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationLayer {
    /// Explicit review policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_policy: Option<ReviewPolicy>,
    /// Explicit review crew.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_crew: Option<String>,
    /// Explicit reviewer starts per candidate lineage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_reviewer_starts: Option<u32>,
    /// Explicit repair cycles per candidate lineage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_repair_cycles: Option<u32>,
    /// Explicit review wall-time minutes per candidate lineage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_minutes: Option<u32>,
}

/// Every live `[operation]` key, as `orbit config keys` and the unknown-key
/// guard see them. Kept sorted so the registry order assertion holds.
pub const OPERATION_KEYS: &[&str] = &[
    "operation.review_crew",
    "operation.review_minutes",
    "operation.review_policy",
    "operation.review_repair_cycles",
    "operation.review_reviewer_starts",
];

impl OperationLayer {
    /// Read the `[operation]` table of one document. Unknown keys and
    /// out-of-range values fail rather than being ignored: a misspelled
    /// review setting that silently resolved to `none` would be a surprising
    /// policy statement. A removed operation-mode key is the exception: the
    /// loader warns about it by name and ignores it, so a `config.toml`
    /// written before the removal keeps loading.
    pub fn from_document(document: &toml::Value, config_path: &Path) -> Result<Self, OrbitError> {
        if let Some(table) = document.as_table().and_then(|table| table.get("operation")) {
            let table = table.as_table().ok_or_else(|| {
                OrbitError::InvalidInput("[operation] must be a table".to_string())
            })?;
            for key in table.keys() {
                let qualified = format!("operation.{key}");
                if !OPERATION_KEYS.contains(&qualified.as_str())
                    && removed_key_note(&qualified).is_none()
                {
                    return Err(OrbitError::InvalidInput(format!(
                        "[operation] has unknown key '{key}'; expected one of: {}",
                        OPERATION_KEYS
                            .iter()
                            .map(|key| key.trim_start_matches("operation."))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                }
            }
        }

        Ok(Self {
            review_policy: read_optional(document, ReviewPolicy::KEY, config_path)?
                .map(|raw: String| ReviewPolicy::parse(&raw))
                .transpose()?,
            review_crew: review_crew(read_optional(
                document,
                "operation.review_crew",
                config_path,
            )?)?,
            review_reviewer_starts: review_reviewer_starts(read_optional(
                document,
                "operation.review_reviewer_starts",
                config_path,
            )?)?,
            review_repair_cycles: review_repair_cycles(read_optional(
                document,
                "operation.review_repair_cycles",
                config_path,
            )?)?,
            review_minutes: review_minutes(read_optional(
                document,
                "operation.review_minutes",
                config_path,
            )?)?,
        })
    }

    /// Whether this layer states anything at all.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The fully resolved review preferences with per-field provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationPolicy {
    /// [`OPERATION_POLICY_VERSION`] at resolution time.
    pub version: u32,
    /// Automatic review timing.
    pub review_policy: OperationField<ReviewPolicy>,
    /// Crew selected for automatic review.
    pub review_crew: OperationField<Option<String>>,
    /// Reviewer starts allowed per delivery candidate lineage.
    pub review_reviewer_starts: OperationField<u32>,
    /// Repair/validation cycles allowed per delivery candidate lineage.
    pub review_repair_cycles: OperationField<u32>,
    /// Aggregate review wall-time minutes per delivery candidate lineage.
    pub review_minutes: OperationField<u32>,
}

impl Default for OperationPolicy {
    fn default() -> Self {
        Self::built_in()
    }
}

impl OperationPolicy {
    /// The built-in layer: no automatic review, default lineage budget.
    pub fn built_in() -> Self {
        Self {
            version: OPERATION_POLICY_VERSION,
            review_policy: OperationField::built_in(ReviewPolicy::None),
            review_crew: OperationField::built_in(None),
            review_reviewer_starts: OperationField::built_in(
                orbit_types::workflow::DEFAULT_REVIEW_REVIEWER_STARTS,
            ),
            review_repair_cycles: OperationField::built_in(
                orbit_types::workflow::DEFAULT_REVIEW_REPAIR_CYCLES,
            ),
            review_minutes: OperationField::built_in(orbit_types::workflow::DEFAULT_REVIEW_MINUTES),
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
        self.review_policy.set(layer.review_policy.as_ref(), source);
        if let Some(crew) = &layer.review_crew {
            self.review_crew = OperationField {
                value: Some(crew.clone()),
                source,
            };
        }
        self.review_reviewer_starts
            .set(layer.review_reviewer_starts.as_ref(), source);
        self.review_repair_cycles
            .set(layer.review_repair_cycles.as_ref(), source);
        self.review_minutes
            .set(layer.review_minutes.as_ref(), source);
    }

    /// The lineage budget the review gate captures at admission.
    pub fn review_budget(&self) -> ReviewBudget {
        ReviewBudget {
            reviewer_starts: self.review_reviewer_starts.value,
            repair_cycles: self.review_repair_cycles.value,
            minutes: self.review_minutes.value,
        }
    }
}

/// Validate `operation.review_policy` for the registry, returning the
/// canonical literal.
pub(crate) fn admit_review_policy(raw: Option<String>) -> Result<Option<String>, OrbitError> {
    raw.map(|value| ReviewPolicy::parse(&value).map(|policy| policy.as_str().to_string()))
        .transpose()
}

pub(crate) fn review_reviewer_starts(raw: Option<u32>) -> Result<Option<u32>, OrbitError> {
    bounded(
        raw,
        "operation.review_reviewer_starts",
        1,
        MAX_REVIEW_REVIEWER_STARTS,
    )
}

pub(crate) fn review_repair_cycles(raw: Option<u32>) -> Result<Option<u32>, OrbitError> {
    bounded(
        raw,
        "operation.review_repair_cycles",
        0,
        MAX_REVIEW_REPAIR_CYCLES,
    )
}

pub(crate) fn review_minutes(raw: Option<u32>) -> Result<Option<u32>, OrbitError> {
    bounded(raw, "operation.review_minutes", 1, MAX_REVIEW_MINUTES)
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
