//! Typed review preferences [ORB-11333] [ORB-13992].
//!
//! Review timing is one key, `review.timing`: `off` (default), `before_pr`
//! (hold PR creation for a reviewer) or `before_landing` (review the open
//! pull request while hosted CI runs on it) [ORB-14849]. There is one review
//! layer before landing, so the two timings are values of one key rather than
//! two switches. Both are bounded by `review.minutes` per candidate, and
//! `workflow.purpose_crews.review` names the crew of automatic review: one
//! crew, or a pool written like the complexity pools that each review draws
//! one crew from [ORB-15195].
//! `workflow.purpose_crews.pre_landing_review`, written the same way, names
//! the before-PR and before-landing reviewer on its own and falls back to
//! `workflow.purpose_crews.review` while unset, so the after-landing review
//! tasks `delivery-code-review` mints can keep a different pool.
//! `review.before_landing_hosts` lets an owner whose own deliveries do not
//! review before landing still capture before-landing review for the claims
//! of the machines it lists [ORB-15192]; it is refused beside
//! `review.timing = "before_pr"` for the same reason. After-landing review has
//! no key here at all: its only switch is the `delivery-code-review`
//! auto-task's own `enabled` flag, which Core reads.
//!
//! Each layer states any subset; a value resolves built-in → global →
//! workspace with its winning layer recorded.
//!
//! `[[review.host_evidence]]` declares the checks a claimed leaf's host owes
//! for the paths its candidate changed ([`HostEvidenceRule`]); a layer that
//! states the list replaces every rule an earlier layer stated.
//!
//! Deprecated spellings are translated when a document is read
//! ([`crate::aliases`]) and warned as deprecated
//! (`registry::DEPRECATED_CONFIG_KEYS`): the `review.before_pr` and
//! `review.before_landing` booleans and the retired `operation.review_policy`
//! enum become `review.timing`, `operation.review_minutes` becomes
//! `review.minutes`, and `operation.review_crew` becomes
//! `workflow.purpose_crews.review`. A legacy `after-landing` policy also
//! enables the auto-task until its flag is set explicitly. The resolved
//! [`OperationPolicy`] keeps its two timing booleans, which run records and
//! claim contracts capture. The retired budget keys
//! (`operation.review_reviewer_starts`, `operation.review_repair_cycles`) are
//! warned and ignored (`registry::REMOVED_CONFIG_KEYS`).

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::identity::validate_machine_id;
use orbit_types::policy::compile_glob_regex;
use orbit_types::task::validate_relative_artifact_path;
use orbit_types::workflow::{
    DEFAULT_REVIEW_MINUTES, EvidenceHostOs, HostEvidenceRule, HostSandboxCommand, ReviewBudget,
    ReviewEvidenceKind,
};
use serde::{Deserialize, Serialize};

use crate::layering::{set_value_at_path, value_at_path};
use crate::registry::read_optional;

/// Version of the resolved-policy shape captured into run records. Bump when
/// a captured field is added, removed, or changes meaning so an older
/// snapshot fails closed for privileged actions instead of being
/// reinterpreted. Version 2 added the independent review budget fields
/// [ORB-11333]; version 3 replaced the review policy enum with
/// `review.before_pr` and dropped the reviewer-start budget [ORB-13992];
/// version 4 added `review.host_evidence`; version 5 added
/// `review.before_landing` [ORB-14849]. `review.before_landing_hosts`
/// [ORB-15192] did not bump it: a captured admission never carries the list,
/// only the timing it resolved for the claim's executor. Nor did the
/// `operation.review_crew` pool [ORB-15195]: a single crew is captured
/// exactly as before, and a pool is captured in a field of its own that an
/// older snapshot never carries. Nor did a rule's `hosted_check`
/// [ORB-15677]: an older snapshot never carries it, and a rule without it
/// keeps the review's hold. Nor did
/// `workflow.purpose_crews.pre_landing_review`: an admission captures the
/// pool it resolved in the fields it always had.
pub const OPERATION_POLICY_VERSION: u32 = 5;

const MAX_REVIEW_MINUTES: u32 = 1_440;

/// When automatic review runs before landing: `off`, `before_pr` or
/// `before_landing`.
pub const REVIEW_TIMING_KEY: &str = "review.timing";
/// The deprecated before-PR switch, translated to [`REVIEW_TIMING_KEY`].
pub const LEGACY_REVIEW_BEFORE_PR_KEY: &str = "review.before_pr";
/// The deprecated before-landing switch, translated to [`REVIEW_TIMING_KEY`].
pub const LEGACY_REVIEW_BEFORE_LANDING_KEY: &str = "review.before_landing";
/// Machines whose claimed leaves review before landing even while the
/// owner's own `review.before_landing` is off.
pub const REVIEW_BEFORE_LANDING_HOSTS_KEY: &str = "review.before_landing_hosts";
/// The before-PR review's time limit.
pub const REVIEW_MINUTES_KEY: &str = "review.minutes";
/// The retired review timing enum, translated on load.
pub const LEGACY_REVIEW_POLICY_KEY: &str = "operation.review_policy";
/// The retired lineage minutes, translated to [`REVIEW_MINUTES_KEY`].
pub const LEGACY_REVIEW_MINUTES_KEY: &str = "operation.review_minutes";

/// Checks a claimed leaf's host owes for the paths it changed.
pub const REVIEW_HOST_EVIDENCE_KEY: &str = "review.host_evidence";

/// The crew, or crew pool, of automatic review: the after-landing review
/// tasks `delivery-code-review` mints, and the reviewer before landing while
/// [`PRE_LANDING_REVIEW_CREW_KEY`] is unset.
pub const REVIEW_CREW_KEY: &str = "workflow.purpose_crews.review";
/// The crew, or crew pool, of before-PR and before-landing review.
pub const PRE_LANDING_REVIEW_CREW_KEY: &str = "workflow.purpose_crews.pre_landing_review";
/// The deprecated spelling of [`REVIEW_CREW_KEY`].
pub const LEGACY_REVIEW_CREW_KEY: &str = "operation.review_crew";

/// `workflow.purpose_crews.review` or `workflow.purpose_crews.pre_landing_review`
/// as written: one crew name, or a pool of `name[:weight]` entries
/// [ORB-15195]. A single name is a one-member pool.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum ReviewCrewSetting {
    /// `review = "sol"`.
    One(String),
    /// `review = ["sol", "grok"]`.
    Pool(Vec<String>),
}

/// `review.timing`: which review layer, if any, runs before landing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewTimingSetting {
    /// No review before landing (the default).
    Off,
    /// Hold PR creation for a reviewer that fixes what it finds.
    BeforePr,
    /// Review the open pull request before it lands.
    BeforeLanding,
}

impl ReviewTimingSetting {
    /// Every value `review.timing` admits, in the order help lists them.
    pub const CHOICES: &'static [&'static str] = &[
        "off",
        "before_pr",
        "before_landing",
        "none",
        "before-pr",
        "before-landing",
        "after_landing",
        "after-landing",
    ];

    /// The persisted spelling, kept readable by the previous release during
    /// a mixed-version deployment. Reading an alias never changes the policy.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::BeforePr => "before_pr",
            Self::BeforeLanding => "before_landing",
        }
    }

    /// The timing a configured spelling names.
    pub fn parse(raw: &str) -> Result<Self, OrbitError> {
        match raw.trim() {
            "off" | "none" | "after_landing" | "after-landing" => Ok(Self::Off),
            "before_pr" | "before-pr" => Ok(Self::BeforePr),
            "before_landing" | "before-landing" => Ok(Self::BeforeLanding),
            other => Err(OrbitError::InvalidInput(format!(
                "{REVIEW_TIMING_KEY} has invalid value '{other}'; expected one of: {}",
                Self::CHOICES.join(", ")
            ))),
        }
    }

    /// Parse a value from a physical config layer, naming it on failure.
    pub(crate) fn parse_in_file(raw: &str, config_path: &Path) -> Result<Self, OrbitError> {
        Self::parse(raw).map_err(|error| match error {
            OrbitError::InvalidInput(message) => OrbitError::InvalidInput(format!(
                "{}: {message}",
                orbit_common::security::redaction::redact_home_dir(
                    &config_path.display().to_string()
                )
            )),
            other => other,
        })
    }

    /// The timing two before-landing switches select. Both on is refused:
    /// there is one review layer before landing.
    pub(crate) fn from_switches(before_pr: bool, before_landing: bool) -> Option<Self> {
        match (before_pr, before_landing) {
            (true, true) => None,
            (true, false) => Some(Self::BeforePr),
            (false, true) => Some(Self::BeforeLanding),
            (false, false) => Some(Self::Off),
        }
    }

    /// The `(before_pr, before_landing)` switches this timing turns on.
    pub fn switches(self) -> (bool, bool) {
        (self == Self::BeforePr, self == Self::BeforeLanding)
    }
}

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

/// What a retired `operation.review_policy` says about before-PR review, or
/// `None` when the document does not state it. [`crate::aliases`] folds it
/// into `review.timing` with the document's other before-landing statements.
pub(crate) fn legacy_policy_before_pr(
    document: &toml::Value,
    config_path: &Path,
) -> Result<Option<bool>, OrbitError> {
    Ok(LegacyReviewPolicy::read(document, config_path)?
        .map(|policy| policy == LegacyReviewPolicy::BeforePr))
}

/// Translate one document's deprecated `operation.review_minutes` into
/// `review.minutes`, in place, unless the document already states it
/// [ORB-13992]. The legacy key stays in the document so the load warns about
/// it by name.
pub(crate) fn translate_legacy_review_minutes(
    document: &mut toml::Value,
    config_path: &Path,
) -> Result<(), OrbitError> {
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
    /// The before-PR half of an explicit `review.timing`, or of its
    /// translation from the deprecated switches or a legacy policy.
    pub review_before_pr: Option<bool>,
    /// The before-landing half of an explicit `review.timing`.
    pub review_before_landing: Option<bool>,
    /// Explicit `review.before_landing_hosts`; an empty list clears the
    /// machines an earlier layer listed.
    pub review_before_landing_hosts: Option<Vec<String>>,
    /// Explicit `review.minutes`, or the legacy `operation.review_minutes`.
    pub review_minutes: Option<u32>,
    /// Explicit review crew pool; a single name is a one-entry pool and an
    /// empty pool unsets a crew an earlier layer named.
    pub review_crew: Option<Vec<String>>,
    /// Explicit pre-landing review crew pool, stated like `review_crew`; an
    /// empty pool unsets one an earlier layer named, so review falls back.
    pub pre_landing_review_crew: Option<Vec<String>>,
    /// Explicit `[[review.host_evidence]]` rules; an empty list clears the
    /// rules an earlier layer stated.
    pub review_host_evidence: Option<Vec<HostEvidenceRule>>,
    /// What a legacy `operation.review_policy` says about after-landing
    /// review: `Some(true)` for `after-landing`, `Some(false)` for any other
    /// value, `None` when the layer does not state the legacy key.
    pub legacy_after_landing: Option<bool>,
}

impl OperationLayer {
    /// Read review preferences. Unknown fields are diagnosed by the config
    /// loader and ignored so different binary versions can share a file.
    /// Known fields still validate their types, values and policy constraints.
    pub fn from_document(document: &toml::Value, config_path: &Path) -> Result<Self, OrbitError> {
        for table in ["operation", "review"] {
            if document.get(table).is_some_and(|value| !value.is_table()) {
                return Err(OrbitError::InvalidInput(format!(
                    "[{table}] must be a table"
                )));
            }
        }

        // The document is translated before it is read, so a deprecated
        // switch already reached `review.timing`; one key states both halves.
        let timing = read_optional::<String>(document, REVIEW_TIMING_KEY, config_path)?
            .map(|raw| ReviewTimingSetting::parse_in_file(&raw, config_path))
            .transpose()?
            .map(ReviewTimingSetting::switches);
        Ok(Self {
            review_before_pr: timing.map(|(before_pr, _)| before_pr),
            review_before_landing: timing.map(|(_, before_landing)| before_landing),
            review_before_landing_hosts: before_landing_hosts(read_optional(
                document,
                REVIEW_BEFORE_LANDING_HOSTS_KEY,
                config_path,
            )?)?,
            review_minutes: review_minutes(read_optional(
                document,
                REVIEW_MINUTES_KEY,
                config_path,
            )?)?,
            review_crew: review_crew(read_optional(document, REVIEW_CREW_KEY, config_path)?)?,
            pre_landing_review_crew: pre_landing_review_crew(read_optional(
                document,
                PRE_LANDING_REVIEW_CREW_KEY,
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

/// The fully resolved review preferences with per-field provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationPolicy {
    /// [`OPERATION_POLICY_VERSION`] at resolution time.
    pub version: u32,
    /// Whether a delivery submitted now holds PR creation for a reviewer.
    pub review_before_pr: OperationField<bool>,
    /// Whether a delivery submitted now reviews its open pull request before
    /// it lands. Never on together with `review_before_pr`.
    #[serde(default = "before_landing_off")]
    pub review_before_landing: OperationField<bool>,
    /// Machines whose claimed leaves review their open pull request before
    /// it lands although `review_before_landing` is off on this owner. Never
    /// non-empty together with `review_before_pr`.
    #[serde(default = "no_before_landing_hosts")]
    pub review_before_landing_hosts: OperationField<Vec<String>>,
    /// Reviewer runtime minutes for one candidate's before-PR or
    /// before-landing review.
    pub review_minutes: OperationField<u32>,
    /// Crews automatic review draws from, as `name[:weight]` entries: the
    /// crew of minted after-landing review tasks, and the before-PR or
    /// before-landing reviewer while `pre_landing_review_crew` is empty.
    /// Empty when unset; one entry for a single crew. Crew names are
    /// admitted against the registry at load.
    pub review_crew: OperationField<Vec<String>>,
    /// Crews the before-PR or before-landing reviewer draws from, in the
    /// same shape; empty when unset. Read it through
    /// [`Self::pre_landing_review_pool`], which applies the fallback.
    #[serde(
        default = "no_pre_landing_review_crew",
        skip_serializing_if = "pre_landing_review_crew_unset"
    )]
    pub pre_landing_review_crew: OperationField<Vec<String>>,
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
            review_before_landing: before_landing_off(),
            review_before_landing_hosts: no_before_landing_hosts(),
            review_minutes: OperationField::built_in(DEFAULT_REVIEW_MINUTES),
            review_crew: OperationField::built_in(Vec::new()),
            pre_landing_review_crew: no_pre_landing_review_crew(),
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

    /// The `review.timing` this resolution selects. Both switches on cannot
    /// come from one key; [`Self::ensure_one_review_layer`] refuses it.
    pub fn review_timing(&self) -> ReviewTimingSetting {
        ReviewTimingSetting::from_switches(
            self.review_before_pr.value,
            self.review_before_landing.value,
        )
        .unwrap_or(ReviewTimingSetting::BeforePr)
    }

    /// Refuse a resolution that turns on both review layers before landing,
    /// naming the layer that set each [ORB-14849]. Listing before-landing
    /// hosts beside before-PR review is the same conflict for those hosts'
    /// claims [ORB-15192].
    pub fn ensure_one_review_layer(&self) -> Result<(), OrbitError> {
        if self.review_before_pr.value && self.review_before_landing.value {
            return Err(OrbitError::InvalidInput(format!(
                "before-PR review ({}) and before-landing review ({}) are both on; there is \
                 one review layer before landing, so set {REVIEW_TIMING_KEY} to one of them",
                self.review_before_pr.source.label(),
                self.review_before_landing.source.label(),
            )));
        }
        if self.review_before_pr.value && !self.review_before_landing_hosts.value.is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "{REVIEW_TIMING_KEY} is before_pr ({}) and {REVIEW_BEFORE_LANDING_HOSTS_KEY} ({}) \
                 lists machines; there is one review layer before landing, so set \
                 {REVIEW_TIMING_KEY} to off or before_landing, or empty the list",
                self.review_before_pr.source.label(),
                self.review_before_landing_hosts.source.label(),
            )));
        }
        Ok(())
    }

    /// Whether a delivery executed on `executor` reviews its open pull
    /// request before it lands: always with `review.timing = before_landing`,
    /// and for a listed machine while the timing is not `before_pr`. `None` is the
    /// owner's own delivery, which the host list never reaches.
    pub fn reviews_before_landing_for(&self, executor: Option<&str>) -> bool {
        self.review_before_landing.value
            || (!self.review_before_pr.value
                && executor.is_some_and(|executor| {
                    self.review_before_landing_hosts
                        .value
                        .iter()
                        .any(|host| host == executor)
                }))
    }

    fn apply_layer(&mut self, source: OperationLayerSource, layer: &OperationLayer) {
        self.review_before_pr
            .set(layer.review_before_pr.as_ref(), source);
        self.review_before_landing
            .set(layer.review_before_landing.as_ref(), source);
        self.review_before_landing_hosts
            .set(layer.review_before_landing_hosts.as_ref(), source);
        self.review_minutes
            .set(layer.review_minutes.as_ref(), source);
        self.review_crew.set(layer.review_crew.as_ref(), source);
        self.pre_landing_review_crew
            .set(layer.pre_landing_review_crew.as_ref(), source);
        self.review_host_evidence
            .set(layer.review_host_evidence.as_ref(), source);
        if let Some(after_landing) = layer.legacy_after_landing {
            self.legacy_after_landing = after_landing.then_some(source);
        }
    }

    /// The pool before-PR and before-landing review draw from:
    /// `workflow.purpose_crews.pre_landing_review` when it names a crew, else
    /// `workflow.purpose_crews.review`. Its entries are empty when neither
    /// names one.
    pub fn pre_landing_review_pool(&self) -> ReviewCrewPool<'_> {
        let (field, key) = if self.pre_landing_review_crew.value.is_empty() {
            (&self.review_crew, REVIEW_CREW_KEY)
        } else {
            (&self.pre_landing_review_crew, PRE_LANDING_REVIEW_CREW_KEY)
        };
        ReviewCrewPool {
            entries: &field.value,
            key,
            source: field.source,
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

/// One review layer's crew pool and the key and layer that supplied it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewCrewPool<'a> {
    /// Canonical `name[:weight]` entries; empty when no key names a crew.
    pub entries: &'a [String],
    /// The key the entries came from, or that a fallback ended at.
    pub key: &'static str,
    /// The layer that set `key`.
    pub source: OperationLayerSource,
}

impl ReviewCrewPool<'_> {
    /// Whether the pool came from `workflow.purpose_crews.review` because
    /// `workflow.purpose_crews.pre_landing_review` names no crew.
    pub fn is_fallback(&self) -> bool {
        self.key == REVIEW_CREW_KEY
    }
}

fn no_pre_landing_review_crew() -> OperationField<Vec<String>> {
    OperationField::built_in(Vec::new())
}

/// An unset pool is not serialized, so a resolution that never states the key
/// reads exactly as it did before the key existed.
fn pre_landing_review_crew_unset(field: &OperationField<Vec<String>>) -> bool {
    field.value.is_empty() && field.source == OperationLayerSource::BuiltIn
}

fn no_host_evidence() -> OperationField<Vec<HostEvidenceRule>> {
    OperationField::built_in(Vec::new())
}

fn before_landing_off() -> OperationField<bool> {
    OperationField::built_in(false)
}

fn no_before_landing_hosts() -> OperationField<Vec<String>> {
    OperationField::built_in(Vec::new())
}

/// Trim, validate and dedupe the listed machine ids. A label that could never
/// match a machine fails the load rather than silently selecting nobody.
pub(crate) fn before_landing_hosts(
    raw: Option<Vec<String>>,
) -> Result<Option<Vec<String>>, OrbitError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let mut hosts = Vec::with_capacity(raw.len());
    for (index, host) in raw.iter().enumerate() {
        let host = host.trim();
        validate_machine_id(host).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "{REVIEW_BEFORE_LANDING_HOSTS_KEY}[{index}] `{host}` is not a machine id: {error}"
            ))
        })?;
        if !hosts.iter().any(|known| known == host) {
            hosts.push(host.to_string());
        }
    }
    Ok(Some(hosts))
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
                hosted_check: rule
                    .hosted_check
                    .as_deref()
                    .map(|check| check.trim().to_string()),
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
            if let Some(check) = &rule.hosted_check {
                if rule.kind != ReviewEvidenceKind::CodeQl || rule.os != EvidenceHostOs::Linux {
                    return Err(invalid(
                        index,
                        "names a hosted_check, which only a linux codeql rule may".to_string(),
                    ));
                }
                if check.is_empty() {
                    return Err(invalid(index, "has an empty hosted_check".to_string()));
                }
            }
            if rule.kind == ReviewEvidenceKind::HostSandboxTest {
                HostSandboxCommand::admit(&rule.command, &[])
                    .map_err(|refusal| invalid(index, format!("command: {}", refusal.detail)))?;
            }
            Ok(rule)
        })
        .collect()
}

/// The pool one layer's `workflow.purpose_crews.review` states. An empty name is
/// refused as before; an empty array states no crew. Entry grammar and crew
/// names are admitted against the registry with the rest of the snapshot.
pub(crate) fn review_crew(
    raw: Option<ReviewCrewSetting>,
) -> Result<Option<Vec<String>>, OrbitError> {
    crew_setting(raw, REVIEW_CREW_KEY)
}

/// The pool one layer's `workflow.purpose_crews.pre_landing_review` states,
/// admitted exactly as [`review_crew`] admits its key.
pub(crate) fn pre_landing_review_crew(
    raw: Option<ReviewCrewSetting>,
) -> Result<Option<Vec<String>>, OrbitError> {
    crew_setting(raw, PRE_LANDING_REVIEW_CREW_KEY)
}

fn crew_setting(
    raw: Option<ReviewCrewSetting>,
    key: &str,
) -> Result<Option<Vec<String>>, OrbitError> {
    match raw {
        Some(ReviewCrewSetting::One(value)) if value.trim().is_empty() => {
            Err(OrbitError::InvalidInput(format!("{key} must not be empty")))
        }
        Some(ReviewCrewSetting::One(value)) => Ok(Some(vec![value.trim().to_string()])),
        Some(ReviewCrewSetting::Pool(entries)) => Ok(Some(entries)),
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
