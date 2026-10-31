//! Selective before-landing review: the captured policy, the host-authored
//! selection receipt, and the pure OR evaluator that turns a pinned
//! candidate diff into a `required` / `not_required` / `unavailable`
//! decision. The receipt is evidence about selection only: a skip or an
//! observation is never a review certificate and creates no after-landing
//! coverage.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::policy::{compile_glob_regex, match_glob, normalize_glob_path};

/// Version of the selective-selection receipt and evaluator semantics.
/// Bump when a change alters a decision for the same policy and diff.
pub const SELECTIVE_REVIEW_RECEIPT_VERSION: u32 = 1;

/// Task artifact carrying the host-authored selection receipt for a
/// delivery lineage. In the reserved `review-` namespace: only system code
/// writes it, never an agent response envelope.
pub const REVIEW_SELECTION_ARTIFACT: &str = "review-selection.json";

/// The most matched paths a receipt spells out; the count of the rest is
/// recorded so the explanation stays bounded without hiding that it is
/// truncated.
pub const SELECTIVE_RECEIPT_MATCH_LIMIT: usize = 64;

/// `review.selective_before_landing.mode`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SelectiveBeforeLandingMode {
    /// No selective scan; today's behavior (the default).
    #[default]
    Off,
    /// Record what would require review; never invoke or enforce.
    Observe,
    /// A positive decision requires the existing before-landing review.
    Enforce,
}

impl SelectiveBeforeLandingMode {
    /// Every accepted spelling, in the order help lists them.
    pub const CHOICES: &'static [&'static str] = &["off", "observe", "enforce"];

    /// The persisted spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Observe => "observe",
            Self::Enforce => "enforce",
        }
    }

    /// Whether the mode evaluates candidates at all.
    pub fn is_active(self) -> bool {
        self != Self::Off
    }
}

/// `review.selective_before_landing.unknown_risk`: what a configured risk
/// predicate with no usable estimate does. It never applies to an
/// unreadable path diff.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SelectiveUnknownRisk {
    /// Treat unknown risk as requiring review (the default).
    #[default]
    Review,
    /// Treat unknown risk as not selecting on the risk predicate.
    Skip,
}

impl SelectiveUnknownRisk {
    /// Every accepted spelling, in the order help lists them.
    pub const CHOICES: &'static [&'static str] = &["review", "skip"];

    /// The persisted spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Skip => "skip",
        }
    }
}

/// The selective before-landing policy an admission captures from owner
/// configuration. Absent on admissions captured before the feature, which
/// keep today's off behavior and are never reinterpreted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SelectiveBeforeLandingPolicy {
    pub mode: SelectiveBeforeLandingMode,
    /// Canonical decimal probability in `[0, 1]`; selection is strictly
    /// `risk > threshold`. `None` disables the risk predicate. Stored as
    /// text so the captured contract stays exactly comparable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_threshold: Option<String>,
    /// Owner-captured model identity, including coefficients or an explicit unknown.
    /// Never supplied by candidate configuration or inferred from a prior receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_model_identity: Option<String>,
    /// Positive repository-relative path globs; empty disables the path
    /// predicate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    pub unknown_risk: SelectiveUnknownRisk,
    /// Which configuration layer decided each field, by dotted key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sources: BTreeMap<String, String>,
    /// SHA-256 of the canonical predicate configuration, so a receipt can
    /// be checked against the policy it claims to have applied.
    pub digest: String,
}

impl SelectiveBeforeLandingPolicy {
    /// Build a policy and seal it with its digest.
    pub fn new(
        mode: SelectiveBeforeLandingMode,
        risk_threshold: Option<String>,
        paths: Vec<String>,
        unknown_risk: SelectiveUnknownRisk,
        sources: BTreeMap<String, String>,
    ) -> Self {
        let digest = Self::digest_of(mode, risk_threshold.as_deref(), &paths, unknown_risk);
        Self {
            mode,
            risk_threshold,
            risk_model_identity: None,
            paths,
            unknown_risk,
            sources,
            digest,
        }
    }

    /// The digest a predicate configuration seals to. Provenance is not
    /// part of it: the same predicates are the same policy whichever layer
    /// stated them.
    pub fn digest_of(
        mode: SelectiveBeforeLandingMode,
        risk_threshold: Option<&str>,
        paths: &[String],
        unknown_risk: SelectiveUnknownRisk,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"selective-before-landing/v1\0");
        hasher.update(mode.as_str().as_bytes());
        hasher.update(b"\0");
        hasher.update(risk_threshold.unwrap_or_default().as_bytes());
        hasher.update(b"\0");
        hasher.update(unknown_risk.as_str().as_bytes());
        for path in paths {
            hasher.update(b"\0");
            hasher.update(path.as_bytes());
        }
        let digest = hasher.finalize();
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        format!("sha256:{hex}")
    }

    /// Whether `other` states the same operator-configured predicates (mode,
    /// risk threshold, paths and unknown-risk behavior). The frozen risk-model
    /// identity, the digest derived from it and the provenance map are not
    /// predicates: a fitted identity records when and from what history the
    /// owner fitted the model, so it can differ across captures as time or
    /// history changes, while an unavailable/no-fit identity carries no
    /// timestamps and may repeat.
    pub fn same_predicates(&self, other: &Self) -> bool {
        self.mode == other.mode
            && self.risk_threshold == other.risk_threshold
            && self.paths == other.paths
            && self.unknown_risk == other.unknown_risk
    }

    /// Freeze the owner's risk snapshot and bind it to the policy digest.
    pub fn freeze_risk_model(&mut self, identity: String) {
        self.risk_model_identity = Some(identity);
        self.digest = self.captured_digest();
    }

    fn captured_digest(&self) -> String {
        let predicates = Self::digest_of(
            self.mode,
            self.risk_threshold.as_deref(),
            &self.paths,
            self.unknown_risk,
        );
        match &self.risk_model_identity {
            None => predicates,
            Some(identity) => {
                let mut hasher = Sha256::new();
                hasher.update(predicates.as_bytes());
                hasher.update(b"\0owner-risk-model\0");
                hasher.update(identity.as_bytes());
                let digest = hasher.finalize();
                let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
                format!("sha256:{hex}")
            }
        }
    }

    /// Whether this policy evaluates candidates.
    pub fn is_active(&self) -> bool {
        self.mode.is_active()
    }

    /// Whether a positive decision under this policy must be enforced.
    pub fn is_enforced(&self) -> bool {
        self.mode == SelectiveBeforeLandingMode::Enforce
    }

    /// The threshold as a probability, when the risk predicate is on.
    pub fn risk_threshold_value(&self) -> Option<f64> {
        self.risk_threshold
            .as_deref()
            .and_then(|raw| raw.parse::<f64>().ok())
            .filter(|value| value.is_finite())
    }

    /// Check the captured policy is well formed and its digest honest.
    /// A policy that fails here must be refused, never dropped.
    pub fn validate(&self) -> Result<(), String> {
        if self.digest != self.captured_digest() {
            return Err("selective policy digest does not match its predicates".to_string());
        }
        if let Some(raw) = &self.risk_threshold {
            validate_risk_threshold(raw)?;
        }
        for pattern in &self.paths {
            validate_selective_path_glob(pattern)?;
        }
        if self.mode.is_active() && self.risk_threshold.is_none() && self.paths.is_empty() {
            return Err(
                "selective before-landing mode is active but configures no predicate: set \
                 risk_threshold or paths"
                    .to_string(),
            );
        }
        Ok(())
    }
}

/// Validate one `risk_threshold` spelling: a finite probability in
/// `[0, 1]`.
pub fn validate_risk_threshold(raw: &str) -> Result<f64, String> {
    let value: f64 = raw
        .trim()
        .parse()
        .map_err(|_| format!("risk_threshold `{raw}` is not a number"))?;
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(format!(
            "risk_threshold `{raw}` must be a finite probability in [0, 1]"
        ));
    }
    Ok(value)
}

/// Validate one selective `paths` glob: a positive, repository-relative
/// pattern. Rejects empty, negated, absolute and parent-traversing
/// patterns, and any pattern the shared glob grammar cannot compile.
pub fn validate_selective_path_glob(pattern: &str) -> Result<(), String> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return Err("path glob must not be empty".to_string());
    }
    if let Some(rest) = pattern.strip_prefix('!') {
        let _ = rest;
        return Err(format!(
            "path glob `{pattern}` is negated; selective paths are positive patterns only"
        ));
    }
    if pattern.starts_with('/') || pattern.starts_with('\\') || pattern.contains('\\') {
        return Err(format!(
            "path glob `{pattern}` must be a repository-relative forward-slash pattern"
        ));
    }
    if pattern
        .split('/')
        .any(|segment| segment == ".." || segment.is_empty())
    {
        return Err(format!(
            "path glob `{pattern}` must stay inside the repository root without empty segments"
        ));
    }
    // The literal segments must normalize as a workspace-relative path;
    // the glob operators themselves are admitted by the compiler below.
    let probe: String = pattern
        .chars()
        .map(|ch| if matches!(ch, '*' | '?') { 'x' } else { ch })
        .collect();
    normalize_glob_path(&probe)
        .map_err(|error| format!("path glob `{pattern}` is not repository-relative: {error}"))?;
    compile_glob_regex(pattern)
        .map_err(|error| format!("path glob `{pattern}` is invalid: {error}"))?;
    Ok(())
}

/// Whether `pattern` matches a Git-recorded path, normalized without
/// following symlinks. An unnormalizable path never matches and is the
/// caller's gap to record, not a non-match to trust.
pub fn selective_path_matches(pattern: &str, path: &str) -> bool {
    match normalize_glob_path(path) {
        Ok(normalized) => match_glob(pattern, &normalized).unwrap_or(false),
        Err(_) => false,
    }
}

/// The outcome of one selection evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectiveReviewResult {
    /// A before-landing review is required (enforced) or would be
    /// (observed).
    Required,
    /// No predicate selected this candidate.
    NotRequired,
    /// The evidence needed to decide could not be read completely.
    Unavailable,
}

impl SelectiveReviewResult {
    /// The recorded spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::NotRequired => "not_required",
            Self::Unavailable => "unavailable",
        }
    }
}

/// One changed path a selective pattern matched, on the old and/or new
/// side of the diff (a rename away from a protected path still matches).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectiveMatchedPath {
    pub path: String,
    pub pattern: String,
    /// `old`, `new`, or `both`.
    pub side: String,
}

/// One task unit's risk evidence on the `diff_lines` basis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectiveTaskRisk {
    pub task_id: String,
    /// Insertions plus deletions measured on the pinned candidate;
    /// `None` when the unit's diff could not be attributed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_lines: Option<u64>,
    /// The fitted probability in canonical decimal spelling, so the
    /// persisted receipt stays exactly comparable; `None` is unknown,
    /// never zero. Read it with [`Self::probability_value`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probability: Option<String>,
    /// Why no usable estimate exists, when `probability` is `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_reason: Option<String>,
}

impl SelectiveTaskRisk {
    /// A risk unit with a fitted probability, spelled canonically.
    pub fn estimated(task_id: String, diff_lines: Option<u64>, probability: f64) -> Self {
        Self {
            task_id,
            diff_lines,
            probability: Some(format!("{probability:.12}")),
            unknown_reason: None,
        }
    }

    /// A risk unit with no usable estimate.
    pub fn unknown(task_id: String, diff_lines: Option<u64>, reason: String) -> Self {
        Self {
            task_id,
            diff_lines,
            probability: None,
            unknown_reason: Some(reason),
        }
    }

    /// The fitted probability, when one was recorded and parses finite.
    pub fn probability_value(&self) -> Option<f64> {
        self.probability
            .as_deref()
            .and_then(|raw| raw.parse::<f64>().ok())
            .filter(|value| value.is_finite())
    }
}

/// The host-authored, versioned record of one selection evaluation at a
/// pinned base/head. Persisted with the gate step output and as the
/// `review-selection.json` task artifact; carried on claimed handoffs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectiveReviewReceipt {
    pub schema_version: u32,
    pub evaluator_version: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub run_id: String,
    pub task_ids: Vec<String>,
    pub policy_digest: String,
    pub mode: SelectiveBeforeLandingMode,
    pub base_sha: String,
    pub base_tree: String,
    pub head_sha: String,
    pub head_tree: String,
    pub result: SelectiveReviewResult,
    /// In observe mode, whether enforcement would have required review;
    /// `None` when the mode enforces (read `result`) or the evidence was
    /// unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub would_require: Option<bool>,
    /// Every matching reason, not just the first (`path:<pattern>`,
    /// `risk_gt_threshold`, `unknown_risk_review`, `previously_selected`).
    pub reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched_paths: Vec<SelectiveMatchedPath>,
    /// Matches beyond [`SELECTIVE_RECEIPT_MATCH_LIMIT`], counted not named.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub matched_paths_omitted: usize,
    /// Changed old/new paths examined.
    pub paths_examined: usize,
    /// Total insertions plus deletions of the whole pinned diff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_lines: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_risks: Vec<SelectiveTaskRisk>,
    /// Frozen owner model identity behind the probabilities (estimator
    /// version, window, support), when the risk predicate ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_model_identity: Option<String>,
    /// Evidence gaps: unreadable diff, truncated scan, missing objects.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<String>,
    /// A stronger host/run policy that also applies, when one does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stronger_policy: Option<String>,
    /// An earlier receipt in this lineage already selected review, so
    /// this revision cannot downgrade the obligation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub previously_selected: bool,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

impl SelectiveReviewReceipt {
    /// Whether enforcement must hold completion for a review under this
    /// receipt.
    pub fn requires_review(&self) -> bool {
        self.mode == SelectiveBeforeLandingMode::Enforce
            && self.result == SelectiveReviewResult::Required
    }

    /// Whether this receipt may be reused for an identical retry: same
    /// policy, base/head and task units.
    pub fn covers(
        &self,
        policy_digest: &str,
        base_sha: &str,
        head_sha: &str,
        task_ids: &[String],
    ) -> bool {
        self.schema_version == SELECTIVE_REVIEW_RECEIPT_VERSION
            && self.policy_digest == policy_digest
            && self.base_sha == base_sha
            && self.head_sha == head_sha
            && self.task_ids == task_ids
    }

    /// A bounded one-line explanation for run/task diagnostics.
    pub fn summary(&self) -> String {
        let observed = match self.would_require {
            Some(true) => " (would require review)",
            Some(false) => " (would not require review)",
            None => "",
        };
        format!(
            "selective before-landing selection {} at {}..{}: {}{}",
            self.result.as_str(),
            short(&self.base_sha),
            short(&self.head_sha),
            if self.reasons.is_empty() {
                "no predicate matched".to_string()
            } else {
                self.reasons.join(", ")
            },
            observed,
        )
    }
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// Inputs the evaluator needs beyond the policy and the pinned diff
/// identities: everything the host measured, with absence explicit.
#[derive(Debug, Clone, Default)]
pub struct SelectiveEvaluationInputs {
    /// `(old_path, new_path)` of every changed entry — deletions carry
    /// only an old path, additions only a new one; renames, binary, mode,
    /// symlink and gitlink entries all appear.
    pub changed_paths: Vec<(Option<String>, Option<String>)>,
    /// Set when the path diff could not be read completely. Selection is
    /// then unavailable regardless of the unknown-risk fallback.
    pub path_diff_gap: Option<String>,
    /// Total insertions plus deletions of the pinned diff.
    pub diff_lines: Option<u64>,
    /// Per-task risk evidence; an entry with no probability is unknown.
    pub task_risks: Vec<SelectiveTaskRisk>,
    /// Frozen owner model identity, when a model was fitted.
    pub risk_model_identity: Option<String>,
    /// An earlier receipt in this lineage selected review.
    pub previously_selected: bool,
    /// A stronger host/run policy that also applies.
    pub stronger_policy: Option<String>,
}

/// Evaluate the selective predicates with OR over a complete pinned diff.
/// Pure: the caller supplies the measured diff and risk evidence and
/// persists the returned receipt.
pub fn evaluate_selective_review(
    policy: &SelectiveBeforeLandingPolicy,
    workspace_id: &str,
    run_id: &str,
    task_ids: &[String],
    base: (&str, &str),
    head: (&str, &str),
    inputs: SelectiveEvaluationInputs,
) -> SelectiveReviewReceipt {
    let mut reasons: Vec<String> = Vec::new();
    let mut gaps: Vec<String> = Vec::new();
    let mut matched: Vec<SelectiveMatchedPath> = Vec::new();
    let mut paths_examined = 0usize;

    if let Some(gap) = &inputs.path_diff_gap {
        gaps.push(gap.clone());
    } else if !policy.paths.is_empty() {
        for (old, new) in &inputs.changed_paths {
            for (side, path) in [("old", old), ("new", new)] {
                let Some(path) = path else { continue };
                paths_examined += 1;
                if normalize_glob_path(path).is_err() {
                    gaps.push(format!(
                        "changed path `{path}` is not a normal repository path"
                    ));
                    continue;
                }
                for pattern in &policy.paths {
                    if selective_path_matches(pattern, path) {
                        match matched
                            .iter_mut()
                            .find(|entry| entry.path == *path && entry.pattern == *pattern)
                        {
                            Some(entry) if entry.side != side => entry.side = "both".to_string(),
                            Some(_) => {}
                            None => matched.push(SelectiveMatchedPath {
                                path: path.clone(),
                                pattern: pattern.clone(),
                                side: side.to_string(),
                            }),
                        }
                        let reason = format!("path:{pattern}");
                        if !reasons.contains(&reason) {
                            reasons.push(reason);
                        }
                    }
                }
            }
        }
    } else {
        paths_examined = inputs
            .changed_paths
            .iter()
            .map(|(old, new)| usize::from(old.is_some()) + usize::from(new.is_some()))
            .sum();
    }

    let mut risk_selected = false;
    let mut risk_unknown = false;
    if let Some(threshold) = policy.risk_threshold_value() {
        if inputs.task_risks.is_empty() {
            risk_unknown = true;
        }
        for risk in &inputs.task_risks {
            match risk.probability_value() {
                Some(probability) => {
                    if probability > threshold {
                        risk_selected = true;
                    }
                }
                None => risk_unknown = true,
            }
        }
        if risk_selected {
            reasons.push("risk_gt_threshold".to_string());
        } else if risk_unknown && policy.unknown_risk == SelectiveUnknownRisk::Review {
            reasons.push("unknown_risk_review".to_string());
        }
    }
    let unknown_selected = policy.risk_threshold.is_some()
        && risk_unknown
        && policy.unknown_risk == SelectiveUnknownRisk::Review;
    if inputs.previously_selected && policy.is_enforced() {
        reasons.push("previously_selected".to_string());
    }

    let matched_paths_omitted = matched.len().saturating_sub(SELECTIVE_RECEIPT_MATCH_LIMIT);
    matched.truncate(SELECTIVE_RECEIPT_MATCH_LIMIT);

    let selected = !matched.is_empty()
        || matched_paths_omitted > 0
        || risk_selected
        || unknown_selected
        || (inputs.previously_selected && policy.is_enforced());
    let result = if !gaps.is_empty() {
        SelectiveReviewResult::Unavailable
    } else if selected {
        SelectiveReviewResult::Required
    } else {
        SelectiveReviewResult::NotRequired
    };
    let would_require = match policy.mode {
        SelectiveBeforeLandingMode::Observe => match result {
            SelectiveReviewResult::Required => Some(true),
            SelectiveReviewResult::NotRequired => Some(false),
            SelectiveReviewResult::Unavailable => None,
        },
        _ => None,
    };

    SelectiveReviewReceipt {
        schema_version: SELECTIVE_REVIEW_RECEIPT_VERSION,
        evaluator_version: SELECTIVE_REVIEW_RECEIPT_VERSION,
        workspace_id: workspace_id.to_string(),
        run_id: run_id.to_string(),
        task_ids: task_ids.to_vec(),
        policy_digest: policy.digest.clone(),
        mode: policy.mode,
        base_sha: base.0.to_string(),
        base_tree: base.1.to_string(),
        head_sha: head.0.to_string(),
        head_tree: head.1.to_string(),
        result,
        would_require,
        reasons,
        matched_paths: matched,
        matched_paths_omitted,
        paths_examined,
        diff_lines: inputs.diff_lines,
        task_risks: inputs.task_risks,
        risk_model_identity: inputs.risk_model_identity,
        gaps,
        stronger_policy: inputs.stronger_policy,
        previously_selected: inputs.previously_selected,
    }
}
