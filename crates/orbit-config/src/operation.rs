//! Typed operation-mode preferences [ORB-11332].
//!
//! A preference describes desired operating defaults; it grants nothing. The
//! durable authorization that lets automation prepare, promote, or complete
//! work is a scoped grant owned by Core and Store, resolved separately.
//!
//! This module owns one rule: preferences resolve built-in supervised →
//! global → workspace → run. At each layer an explicit `preset` selection
//! resets every preset-managed field to that preset's defaults *before* the
//! layer's own explicit fields apply, so a workspace choosing `supervised`
//! cannot inherit a hidden global autonomous completion preference. The
//! review fields and the delivery cap are independent: a preset selection
//! neither resets nor infers them. Every resolved field records the layer
//! that decided it and whether the value is a preset default.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::{ReviewBudget, ReviewTiming};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};

use crate::registry::read_optional;

/// Version of the resolved-policy shape captured into run and grant records.
/// Bump when a field is added, removed, or changes meaning so an older
/// snapshot fails closed for privileged actions instead of being reinterpreted.
/// Version 2 added the independent review budget fields [ORB-11333].
pub const OPERATION_POLICY_VERSION: u32 = 2;

/// Candidate default due interval for automatic preparation, in seconds.
pub const DEFAULT_PREPARATION_DUE_SECONDS: u64 = 300;
/// Candidate default recovery episodes per task.
pub const DEFAULT_RECOVERY_EPISODES_PER_TASK: u32 = 2;
/// Candidate default aggregate recovery allowance per task, in minutes.
pub const DEFAULT_RECOVERY_MINUTES_PER_TASK: u32 = 30;
const SUPERVISED_LEAF_CEILING: u32 = 5;
const AUTONOMOUS_LEAF_CEILING: u32 = 10;

const MAX_PREPARATION_DUE_SECONDS: u64 = 86_400;
const MAX_LEAF_CEILING: u32 = 500;
const MAX_RECOVERY_EPISODES_PER_TASK: u32 = 10;
const MAX_RECOVERY_MINUTES_PER_TASK: u32 = 1_440;
const MAX_REVIEW_REVIEWER_STARTS: u32 = 10;
const MAX_REVIEW_REPAIR_CYCLES: u32 = 10;
const MAX_REVIEW_MINUTES: u32 = 1_440;

macro_rules! choice_enum {
    (
        $(#[$meta:meta])*
        $name:ident, $key:literal, { $( $(#[$vmeta:meta])* $variant:ident => $literal:literal ),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "kebab-case")]
        pub enum $name {
            $( $(#[$vmeta])* #[serde(rename = $literal)] $variant, )+
        }

        impl $name {
            /// The configuration key this choice is read from.
            pub const KEY: &'static str = $key;
            /// Every accepted literal, in declaration order.
            pub const CHOICES: &'static [&'static str] = &[ $( $literal, )+ ];

            /// The literal spelling written in `config.toml`.
            pub fn as_str(self) -> &'static str {
                match self {
                    $( Self::$variant => $literal, )+
                }
            }

            /// Parse a literal, naming the key and the accepted values on failure.
            pub fn parse(raw: &str) -> Result<Self, OrbitError> {
                match raw.trim() {
                    $( $literal => Ok(Self::$variant), )+
                    other => Err(OrbitError::InvalidInput(format!(
                        "{} has invalid value '{other}'; expected one of: {}",
                        Self::KEY,
                        Self::CHOICES.join(", ")
                    ))),
                }
            }
        }
    };
}

choice_enum! {
    /// The operating preset: how authorized work advances, never who may authorize it.
    OperationPreset, "operation.preset", {
        /// Explicit pilot runs, separate approval, review handoff.
        Supervised => "supervised",
        /// Scheduled preparation, grant-bound promotion and completion requests.
        Autonomous => "autonomous",
    }
}

choice_enum! {
    /// Whether missing/stale preparation is scheduled automatically inside a grant.
    PreparationPreference, "operation.preparation", {
        /// Only explicit pilot runs and independently enabled routines.
        Manual => "manual",
        /// In-grant tasks become due after `preparation_due_seconds`.
        Automatic => "automatic",
    }
}

choice_enum! {
    /// Whether fresh positive assessments may promote proposed work inside a grant.
    PromotionPreference, "operation.promotion", {
        /// Proposed work waits for a separate approval.
        SeparateApproval => "separate_approval",
        /// Fresh positive evidence plus a promote right moves proposed work to backlog.
        Automatic => "automatic",
    }
}

choice_enum! {
    /// Requested delivery end state; the delivery cap and the grant bound it.
    CompletionPreference, "operation.completion", {
        /// Stop at the delivery handoff.
        Review => "review",
        /// Request the guarded `review -> done` transition within the grant.
        Done => "done",
    }
}

choice_enum! {
    /// Whether bounded recovery is scheduled automatically inside a grant.
    RecoveryPreference, "operation.recovery", {
        /// Existing configured step recovery and explicitly enabled triage only.
        Existing => "existing",
        /// In-grant incidents are scheduled promptly, within the aggregate budget.
        Scheduled => "scheduled",
    }
}

choice_enum! {
    /// When automatic code review applies. Independent of the preset.
    ReviewPolicy, "operation.review_policy", {
        /// No automatic review managed by this policy.
        None => "none",
        /// Hold PR creation for a fresh reviewer with scoped repairs [ORB-11333].
        BeforePr => "before-pr",
        /// Accumulate uncovered landed deliveries for a scheduled review.
        AfterLanding => "after-landing",
    }
}

choice_enum! {
    /// Repository-level ceiling on managed delivery. Independent of the preset.
    DeliveryCap, "operation.delivery_cap", {
        /// Managed delivery stops at the PR/handoff even under an autonomous preference.
        Review => "review",
        /// The repository allows managed completion when a grant carries the right.
        Done => "done",
    }
}

impl ReviewPolicy {
    /// The shared review-timing contract this preference selects.
    pub fn timing(self) -> ReviewTiming {
        match self {
            ReviewPolicy::None => ReviewTiming::None,
            ReviewPolicy::BeforePr => ReviewTiming::BeforePr,
            ReviewPolicy::AfterLanding => ReviewTiming::AfterLanding,
        }
    }
}

/// The preset-managed defaults one preset selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresetDefaults {
    /// Preparation scheduling preference.
    pub preparation: PreparationPreference,
    /// Due interval for automatic preparation, in seconds.
    pub preparation_due_seconds: u64,
    /// Ceiling on concurrently live leaf runs.
    pub leaf_ceiling: u32,
    /// Promotion preference.
    pub promotion: PromotionPreference,
    /// Completion preference.
    pub completion: CompletionPreference,
    /// Recovery scheduling preference.
    pub recovery: RecoveryPreference,
    /// Recovery episodes allowed per task.
    pub recovery_episodes_per_task: u32,
    /// Recovery wall-time minutes allowed per task.
    pub recovery_minutes_per_task: u32,
}

impl OperationPreset {
    /// The defaults this preset installs for every preset-managed field.
    pub fn defaults(self) -> PresetDefaults {
        match self {
            OperationPreset::Supervised => PresetDefaults {
                preparation: PreparationPreference::Manual,
                preparation_due_seconds: DEFAULT_PREPARATION_DUE_SECONDS,
                leaf_ceiling: SUPERVISED_LEAF_CEILING,
                promotion: PromotionPreference::SeparateApproval,
                completion: CompletionPreference::Review,
                recovery: RecoveryPreference::Existing,
                recovery_episodes_per_task: DEFAULT_RECOVERY_EPISODES_PER_TASK,
                recovery_minutes_per_task: DEFAULT_RECOVERY_MINUTES_PER_TASK,
            },
            OperationPreset::Autonomous => PresetDefaults {
                preparation: PreparationPreference::Automatic,
                preparation_due_seconds: DEFAULT_PREPARATION_DUE_SECONDS,
                leaf_ceiling: AUTONOMOUS_LEAF_CEILING,
                promotion: PromotionPreference::Automatic,
                completion: CompletionPreference::Done,
                recovery: RecoveryPreference::Scheduled,
                recovery_episodes_per_task: DEFAULT_RECOVERY_EPISODES_PER_TASK,
                recovery_minutes_per_task: DEFAULT_RECOVERY_MINUTES_PER_TASK,
            },
        }
    }
}

/// Which layer supplied a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OperationLayerSource {
    /// The crate's built-in supervised defaults.
    BuiltIn,
    /// The global `config.toml`.
    Global,
    /// The workspace `config.toml`.
    Workspace,
    /// Explicit overrides supplied for one enable/explain request.
    Run,
}

impl OperationLayerSource {
    /// The label shown in explanations.
    pub fn label(self) -> &'static str {
        match self {
            OperationLayerSource::BuiltIn => "built-in",
            OperationLayerSource::Global => "global",
            OperationLayerSource::Workspace => "workspace",
            OperationLayerSource::Run => "run",
        }
    }
}

/// Where a resolved field's value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationFieldSource {
    /// The layer whose statement won.
    pub layer: OperationLayerSource,
    /// Set when the value is a preset default installed by selecting this
    /// preset at `layer`, rather than an explicit field at that layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<OperationPreset>,
}

impl OperationFieldSource {
    fn explicit(layer: OperationLayerSource) -> Self {
        Self {
            layer,
            preset: None,
        }
    }

    fn preset(layer: OperationLayerSource, preset: OperationPreset) -> Self {
        Self {
            layer,
            preset: Some(preset),
        }
    }

    /// Human label: `workspace`, or `preset:autonomous@global`.
    pub fn label(&self) -> String {
        match self.preset {
            Some(preset) => format!("preset:{}@{}", preset.as_str(), self.layer.label()),
            None => self.layer.label().to_string(),
        }
    }
}

/// One resolved field with its provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationField<T> {
    /// The resolved value.
    pub value: T,
    /// The layer that decided it.
    pub source: OperationFieldSource,
}

impl<T: Copy> OperationField<T> {
    fn set(&mut self, value: Option<T>, layer: OperationLayerSource) {
        if let Some(value) = value {
            self.value = value;
            self.source = OperationFieldSource::explicit(layer);
        }
    }
}

/// The explicit `[operation]` statements one layer makes. Every field is
/// optional: an omitted field inherits, an omitted preset preserves inherited
/// preset-managed fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationLayer {
    /// Explicit preset selection; resets the preset-managed fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<OperationPreset>,
    /// Explicit preparation preference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation: Option<PreparationPreference>,
    /// Explicit preparation due interval, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation_due_seconds: Option<u64>,
    /// Explicit leaf-run ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaf_ceiling: Option<u32>,
    /// Explicit promotion preference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promotion: Option<PromotionPreference>,
    /// Explicit completion preference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<CompletionPreference>,
    /// Explicit recovery preference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<RecoveryPreference>,
    /// Explicit recovery episodes per task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_episodes_per_task: Option<u32>,
    /// Explicit recovery minutes per task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_minutes_per_task: Option<u32>,
    /// Explicit review policy (independent field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_policy: Option<ReviewPolicy>,
    /// Explicit review crew (independent field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_crew: Option<String>,
    /// Explicit reviewer starts per candidate lineage (independent field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_reviewer_starts: Option<u32>,
    /// Explicit repair cycles per candidate lineage (independent field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_repair_cycles: Option<u32>,
    /// Explicit review wall-time minutes per candidate lineage (independent field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_minutes: Option<u32>,
    /// Explicit delivery cap (independent field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_cap: Option<DeliveryCap>,
}

/// Every `[operation]` key, as `orbit config keys` and the unknown-key guard
/// see them. Kept sorted so the registry order assertion holds.
pub const OPERATION_KEYS: &[&str] = &[
    "operation.completion",
    "operation.delivery_cap",
    "operation.leaf_ceiling",
    "operation.preparation",
    "operation.preparation_due_seconds",
    "operation.preset",
    "operation.promotion",
    "operation.recovery",
    "operation.recovery_episodes_per_task",
    "operation.recovery_minutes_per_task",
    "operation.review_crew",
    "operation.review_minutes",
    "operation.review_policy",
    "operation.review_repair_cycles",
    "operation.review_reviewer_starts",
];

/// Keys an explicit preset selection resets to that preset's defaults.
pub const PRESET_MANAGED_KEYS: &[&str] = &[
    "operation.completion",
    "operation.leaf_ceiling",
    "operation.preparation",
    "operation.preparation_due_seconds",
    "operation.promotion",
    "operation.recovery",
    "operation.recovery_episodes_per_task",
    "operation.recovery_minutes_per_task",
];

impl OperationLayer {
    /// Read the `[operation]` table of one document. Unknown keys and
    /// out-of-range values fail rather than being ignored: a misspelled
    /// autonomous setting that silently resolved to supervised (or the
    /// reverse) would be a surprising authority statement.
    pub fn from_document(document: &toml::Value, config_path: &Path) -> Result<Self, OrbitError> {
        if let Some(table) = document.as_table().and_then(|table| table.get("operation")) {
            let table = table.as_table().ok_or_else(|| {
                OrbitError::InvalidInput("[operation] must be a table".to_string())
            })?;
            for key in table.keys() {
                let qualified = format!("operation.{key}");
                if !OPERATION_KEYS.contains(&qualified.as_str()) {
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
            preset: parse_choice::<OperationPreset>(read_optional(
                document,
                OperationPreset::KEY,
                config_path,
            )?)?,
            preparation: parse_choice::<PreparationPreference>(read_optional(
                document,
                PreparationPreference::KEY,
                config_path,
            )?)?,
            preparation_due_seconds: preparation_due_seconds(read_optional(
                document,
                "operation.preparation_due_seconds",
                config_path,
            )?)?,
            leaf_ceiling: leaf_ceiling(read_optional(
                document,
                "operation.leaf_ceiling",
                config_path,
            )?)?,
            promotion: parse_choice::<PromotionPreference>(read_optional(
                document,
                PromotionPreference::KEY,
                config_path,
            )?)?,
            completion: parse_choice::<CompletionPreference>(read_optional(
                document,
                CompletionPreference::KEY,
                config_path,
            )?)?,
            recovery: parse_choice::<RecoveryPreference>(read_optional(
                document,
                RecoveryPreference::KEY,
                config_path,
            )?)?,
            recovery_episodes_per_task: recovery_episodes_per_task(read_optional(
                document,
                "operation.recovery_episodes_per_task",
                config_path,
            )?)?,
            recovery_minutes_per_task: recovery_minutes_per_task(read_optional(
                document,
                "operation.recovery_minutes_per_task",
                config_path,
            )?)?,
            review_policy: parse_choice::<ReviewPolicy>(read_optional(
                document,
                ReviewPolicy::KEY,
                config_path,
            )?)?,
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
            delivery_cap: parse_choice::<DeliveryCap>(read_optional(
                document,
                DeliveryCap::KEY,
                config_path,
            )?)?,
        })
    }

    /// Whether this layer states anything at all.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The fully resolved preferences with per-field provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationPolicy {
    /// [`OPERATION_POLICY_VERSION`] at resolution time.
    pub version: u32,
    /// The winning preset.
    pub preset: OperationField<OperationPreset>,
    /// Preparation scheduling preference.
    pub preparation: OperationField<PreparationPreference>,
    /// Due interval for automatic preparation, in seconds.
    pub preparation_due_seconds: OperationField<u64>,
    /// Ceiling on concurrently live leaf runs.
    pub leaf_ceiling: OperationField<u32>,
    /// Promotion preference.
    pub promotion: OperationField<PromotionPreference>,
    /// Completion preference before the delivery cap and grant apply.
    pub completion: OperationField<CompletionPreference>,
    /// Recovery scheduling preference.
    pub recovery: OperationField<RecoveryPreference>,
    /// Recovery episodes allowed per task.
    pub recovery_episodes_per_task: OperationField<u32>,
    /// Recovery wall-time minutes allowed per task.
    pub recovery_minutes_per_task: OperationField<u32>,
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
    /// Repository ceiling on managed delivery.
    pub delivery_cap: OperationField<DeliveryCap>,
}

impl Default for OperationPolicy {
    fn default() -> Self {
        Self::built_in()
    }
}

impl OperationPolicy {
    /// The built-in layer: supervised, no automatic review, and delivery
    /// capped at review until a repository explicitly raises it.
    pub fn built_in() -> Self {
        let layer = OperationLayerSource::BuiltIn;
        let mut policy = Self {
            version: OPERATION_POLICY_VERSION,
            preset: OperationField {
                value: OperationPreset::Supervised,
                source: OperationFieldSource::explicit(layer),
            },
            preparation: placeholder(PreparationPreference::Manual),
            preparation_due_seconds: placeholder(DEFAULT_PREPARATION_DUE_SECONDS),
            leaf_ceiling: placeholder(SUPERVISED_LEAF_CEILING),
            promotion: placeholder(PromotionPreference::SeparateApproval),
            completion: placeholder(CompletionPreference::Review),
            recovery: placeholder(RecoveryPreference::Existing),
            recovery_episodes_per_task: placeholder(DEFAULT_RECOVERY_EPISODES_PER_TASK),
            recovery_minutes_per_task: placeholder(DEFAULT_RECOVERY_MINUTES_PER_TASK),
            review_policy: OperationField {
                value: ReviewPolicy::None,
                source: OperationFieldSource::explicit(layer),
            },
            review_crew: OperationField {
                value: None,
                source: OperationFieldSource::explicit(layer),
            },
            review_reviewer_starts: placeholder(
                orbit_types::workflow::DEFAULT_REVIEW_REVIEWER_STARTS,
            ),
            review_repair_cycles: placeholder(orbit_types::workflow::DEFAULT_REVIEW_REPAIR_CYCLES),
            review_minutes: placeholder(orbit_types::workflow::DEFAULT_REVIEW_MINUTES),
            delivery_cap: OperationField {
                value: DeliveryCap::Review,
                source: OperationFieldSource::explicit(layer),
            },
        };
        policy.select_preset(OperationPreset::Supervised, layer);
        policy
    }

    /// Resolve the layers in precedence order over the built-in defaults.
    pub fn resolve(layers: &[(OperationLayerSource, &OperationLayer)]) -> Self {
        let mut policy = Self::built_in();
        for (source, layer) in layers {
            policy.apply_layer(*source, layer);
        }
        policy
    }

    /// Apply explicit run-time overrides as the final layer.
    pub fn with_run_layer(&self, run: &OperationLayer) -> Self {
        let mut policy = self.clone();
        policy.apply_layer(OperationLayerSource::Run, run);
        policy
    }

    fn apply_layer(&mut self, source: OperationLayerSource, layer: &OperationLayer) {
        if let Some(preset) = layer.preset {
            self.preset = OperationField {
                value: preset,
                source: OperationFieldSource::explicit(source),
            };
            self.select_preset(preset, source);
        }

        self.preparation.set(layer.preparation, source);
        self.preparation_due_seconds
            .set(layer.preparation_due_seconds, source);
        self.leaf_ceiling.set(layer.leaf_ceiling, source);
        self.promotion.set(layer.promotion, source);
        self.completion.set(layer.completion, source);
        self.recovery.set(layer.recovery, source);
        self.recovery_episodes_per_task
            .set(layer.recovery_episodes_per_task, source);
        self.recovery_minutes_per_task
            .set(layer.recovery_minutes_per_task, source);

        // Independent fields: their own precedence, never a preset reset.
        self.review_policy.set(layer.review_policy, source);
        if let Some(crew) = &layer.review_crew {
            self.review_crew = OperationField {
                value: Some(crew.clone()),
                source: OperationFieldSource::explicit(source),
            };
        }
        self.review_reviewer_starts
            .set(layer.review_reviewer_starts, source);
        self.review_repair_cycles
            .set(layer.review_repair_cycles, source);
        self.review_minutes.set(layer.review_minutes, source);
        self.delivery_cap.set(layer.delivery_cap, source);
    }

    /// Reset every preset-managed field to `preset`'s defaults.
    fn select_preset(&mut self, preset: OperationPreset, layer: OperationLayerSource) {
        let defaults = preset.defaults();
        let source = OperationFieldSource::preset(layer, preset);
        self.preparation = OperationField {
            value: defaults.preparation,
            source,
        };
        self.preparation_due_seconds = OperationField {
            value: defaults.preparation_due_seconds,
            source,
        };
        self.leaf_ceiling = OperationField {
            value: defaults.leaf_ceiling,
            source,
        };
        self.promotion = OperationField {
            value: defaults.promotion,
            source,
        };
        self.completion = OperationField {
            value: defaults.completion,
            source,
        };
        self.recovery = OperationField {
            value: defaults.recovery,
            source,
        };
        self.recovery_episodes_per_task = OperationField {
            value: defaults.recovery_episodes_per_task,
            source,
        };
        self.recovery_minutes_per_task = OperationField {
            value: defaults.recovery_minutes_per_task,
            source,
        };
    }

    /// The completion the repository cap allows for this policy, with the
    /// cap disclosed when it reduces the preference.
    pub fn capped_completion(&self) -> (CompletionPreference, Option<&'static str>) {
        match (self.completion.value, self.delivery_cap.value) {
            (CompletionPreference::Done, DeliveryCap::Review) => {
                (CompletionPreference::Review, Some("delivery_cap_review"))
            }
            (completion, _) => (completion, None),
        }
    }

    /// The lineage budget the review gate captures at admission.
    pub fn review_budget(&self) -> ReviewBudget {
        ReviewBudget {
            reviewer_starts: self.review_reviewer_starts.value,
            repair_cycles: self.review_repair_cycles.value,
            minutes: self.review_minutes.value,
        }
    }

    /// The explanation view: every field with its value and winning source.
    pub fn explain(&self) -> JsonValue {
        fn field<T: Serialize>(field: &OperationField<T>) -> JsonValue {
            json!({ "value": field.value, "source": field.source.label() })
        }
        let (effective_completion, cap_reason) = self.capped_completion();
        json!({
            "version": self.version,
            "preset": field(&self.preset),
            "preparation": field(&self.preparation),
            "preparation_due_seconds": field(&self.preparation_due_seconds),
            "leaf_ceiling": field(&self.leaf_ceiling),
            "promotion": field(&self.promotion),
            "completion": field(&self.completion),
            "recovery": field(&self.recovery),
            "recovery_episodes_per_task": field(&self.recovery_episodes_per_task),
            "recovery_minutes_per_task": field(&self.recovery_minutes_per_task),
            "review_policy": field(&self.review_policy),
            "review_crew": field(&self.review_crew),
            "review_reviewer_starts": field(&self.review_reviewer_starts),
            "review_repair_cycles": field(&self.review_repair_cycles),
            "review_minutes": field(&self.review_minutes),
            "delivery_cap": field(&self.delivery_cap),
            "effective_completion": {
                "value": effective_completion,
                "cap": cap_reason,
            },
        })
    }
}

fn placeholder<T>(value: T) -> OperationField<T> {
    OperationField {
        value,
        source: OperationFieldSource::explicit(OperationLayerSource::BuiltIn),
    }
}

fn parse_choice<T>(raw: Option<String>) -> Result<Option<T>, OrbitError>
where
    T: ChoiceParse,
{
    raw.map(|value| T::parse_choice(&value)).transpose()
}

/// Shared parse entry point so registry rows and the layer reader validate
/// through one path.
pub trait ChoiceParse: Sized {
    /// Parse one literal, naming the key on failure.
    fn parse_choice(raw: &str) -> Result<Self, OrbitError>;
}

macro_rules! impl_choice_parse {
    ($($name:ident),+ $(,)?) => {
        $(
            impl ChoiceParse for $name {
                fn parse_choice(raw: &str) -> Result<Self, OrbitError> {
                    Self::parse(raw)
                }
            }
        )+
    };
}

impl_choice_parse!(
    OperationPreset,
    PreparationPreference,
    PromotionPreference,
    CompletionPreference,
    RecoveryPreference,
    ReviewPolicy,
    DeliveryCap,
);

/// Validate a choice key for the registry, returning the canonical literal.
pub(crate) fn admit_choice<T: ChoiceParse + Copy + 'static>(
    raw: Option<String>,
    render: fn(T) -> &'static str,
) -> Result<Option<String>, OrbitError> {
    Ok(parse_choice::<T>(raw)?.map(|value| render(value).to_string()))
}

pub(crate) fn preparation_due_seconds(raw: Option<u64>) -> Result<Option<u64>, OrbitError> {
    bounded(
        raw,
        "operation.preparation_due_seconds",
        1,
        MAX_PREPARATION_DUE_SECONDS,
    )
}

pub(crate) fn leaf_ceiling(raw: Option<u32>) -> Result<Option<u32>, OrbitError> {
    bounded(raw, "operation.leaf_ceiling", 1, MAX_LEAF_CEILING)
}

pub(crate) fn recovery_episodes_per_task(raw: Option<u32>) -> Result<Option<u32>, OrbitError> {
    bounded(
        raw,
        "operation.recovery_episodes_per_task",
        0,
        MAX_RECOVERY_EPISODES_PER_TASK,
    )
}

pub(crate) fn recovery_minutes_per_task(raw: Option<u32>) -> Result<Option<u32>, OrbitError> {
    bounded(
        raw,
        "operation.recovery_minutes_per_task",
        1,
        MAX_RECOVERY_MINUTES_PER_TASK,
    )
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
