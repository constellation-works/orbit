use orbit_common::OrbitError;
use orbit_types::workflow::{DEFAULT_REVIEW_MINUTES, HostEvidenceRule, ReviewBudget};
use serde::{Deserialize, Serialize};

use super::crew::ReviewCrewPool;
use super::keys::{
    PRE_LANDING_REVIEW_CREW_KEY, REVIEW_BEFORE_LANDING_HOSTS_KEY, REVIEW_CREW_KEY,
    REVIEW_TIMING_KEY,
};
use super::layer::{OperationField, OperationLayer, OperationLayerSource};
use super::timing::ReviewTimingSetting;

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
/// pool it resolved in the fields it always had. Nor did
/// `review.selective_before_landing`, nor its removal [ORB-22808]: a
/// snapshot that still carries its resolved fields loads, and they are
/// ignored.
pub const OPERATION_POLICY_VERSION: u32 = 5;

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

    /// Copy the canonical admitted review pools — the snapshot's
    /// `workflow.purpose_crews.review` and `workflow.purpose_crews.pre_landing_review`
    /// — into this policy, keeping each field's existing source. An unset
    /// pool becomes empty.
    pub(crate) fn project_canonical_review_pools(
        &mut self,
        review: Option<&[String]>,
        pre_landing_review: Option<&[String]>,
    ) {
        self.review_crew.value = review.map(<[String]>::to_vec).unwrap_or_default();
        self.pre_landing_review_crew.value = pre_landing_review
            .map(<[String]>::to_vec)
            .unwrap_or_default();
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
