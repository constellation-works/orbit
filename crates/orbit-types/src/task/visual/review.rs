//! Visual review's comparison findings and their in-line verification
//! (visual-review design §8, §10, §11): the system-written
//! `visual/review.json`, image-region evidence and its validation, image
//! dimensions and per-provider token estimates, and the budget selection
//! that decides which mocks one review invocation is shown.
//!
//! Everything here is pure: callers supply image bytes, the capture of
//! record and the budgets, and every refusal is named. In `shadow` the
//! record is evidence only; in `live` each confirmed finding also carries
//! its disposition under `visual_review.block_at` and `file_at` (§11.3)
//! and, once the code reviewer has seen it, its resolution (§1).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    VisualCaptureRecord, VisualNotEstablished, VisualNotShownMock, VisualReviewMode,
    VisualShownMock,
};

/// The comparison review of record (§11).
pub const VISUAL_REVIEW_ARTIFACT: &str = "visual/review.json";
/// Images sent to one review invocation, canary included, when
/// `visual_review.max_images` is unset.
pub const DEFAULT_VISUAL_MAX_IMAGES: u32 = 32;
/// Estimated image tokens for one review invocation when
/// `visual_review.max_image_tokens` is unset.
pub const DEFAULT_VISUAL_MAX_IMAGE_TOKENS: u64 = 48_000;
/// Findings accepted from one review when `visual_review.max_findings`
/// is unset; extras are dropped and counted.
pub const DEFAULT_VISUAL_MAX_FINDINGS: u32 = 15;
/// Findings per verifier invocation (§10, §11.2).
pub const VISUAL_VERIFY_BATCH_SIZE: usize = 5;
/// Per-image token constant for a provider without a published rule (§10).
pub const VISUAL_IMAGE_TOKEN_CONSTANT: u64 = 1_500;
/// The purpose every comparison-review record and crew draw carries
/// (§11.4): reviewer scorecards read it to attribute the findings.
pub const VISUAL_REVIEW_PURPOSE: &str = "visual_review";
/// Why a review is not established when no crew can take images (§9.4).
pub const VISUAL_NO_VISION_CREW: &str = "no_vision_crew";
/// The purpose of the post-landing comparison of the landed head (§12.2):
/// its records and invocations carry it, and the experiment cost metric
/// leaves them out.
pub const VISUAL_MEASURE_PURPOSE: &str = "visual_measure";
/// The tag every follow-up visual review files carries (§11.3).
pub const VISUAL_REVIEW_TAG: &str = "visual-review";
/// The history event an operator's acceptance of a disputed blocking
/// finding writes (§1).
pub const VISUAL_DISPUTE_ACCEPTED_EVENT: &str = "visual_dispute_accepted";
/// The workers of the post-landing comparison (§12.2). The experiment
/// cost metric excludes their invocations.
pub const VISUAL_MEASURE_INVOCATION_IDS: [&str; 2] =
    ["visual_measure_reviewer", "visual_measure_verifier"];

/// The rank of a shared-rubric severity: `critical` 4 down to `nit` 0.
/// `None` for anything else.
#[must_use]
pub fn visual_severity_rank(severity: &str) -> Option<u8> {
    match severity.trim() {
        "critical" => Some(4),
        "high" => Some(3),
        "medium" => Some(2),
        "low" => Some(1),
        "nit" => Some(0),
        _ => None,
    }
}

/// The severity weight a confirmed finding scores (§11.4, §12.3):
/// 8, 4, 2, 1 and 0.25 from `critical` down to `nit`.
#[must_use]
pub fn visual_severity_weight(severity: &str) -> f64 {
    match visual_severity_rank(severity) {
        Some(4) => 8.0,
        Some(3) => 4.0,
        Some(2) => 2.0,
        Some(1) => 1.0,
        Some(_) => 0.25,
        None => 0.0,
    }
}

/// `visual_review.block_at` and `visual_review.file_at` (§11.3): the
/// lowest confirmed severity that blocks or is filed, or `never`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualThreshold {
    Critical,
    High,
    Medium,
    Low,
    Nit,
    Never,
}

/// `visual_review.block_at` when unset.
pub const DEFAULT_VISUAL_BLOCK_AT: VisualThreshold = VisualThreshold::High;
/// `visual_review.file_at` when unset.
pub const DEFAULT_VISUAL_FILE_AT: VisualThreshold = VisualThreshold::Low;

impl VisualThreshold {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Nit => "nit",
            Self::Never => "never",
        }
    }

    /// Parse a configured value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "critical" => Some(Self::Critical),
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            "nit" => Some(Self::Nit),
            "never" => Some(Self::Never),
            _ => None,
        }
    }

    /// Its rank on the [`visual_severity_rank`] scale; `never` is above
    /// every severity.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Critical => 4,
            Self::High => 3,
            Self::Medium => 2,
            Self::Low => 1,
            Self::Nit => 0,
            Self::Never => 5,
        }
    }

    /// Whether a finding of `severity` is at or above this threshold.
    #[must_use]
    pub fn admits(self, severity: &str) -> bool {
        self != Self::Never
            && visual_severity_rank(severity).is_some_and(|rank| rank >= self.rank())
    }
}

pub(super) fn default_block_at() -> VisualThreshold {
    DEFAULT_VISUAL_BLOCK_AT
}

pub(super) fn default_file_at() -> VisualThreshold {
    DEFAULT_VISUAL_FILE_AT
}

// A contract with the default thresholds serializes as before, so older
// readers, which deny unknown fields, still decode it.
#[allow(clippy::trivially_copy_pass_by_ref)]
pub(super) fn is_default_block_at(threshold: &VisualThreshold) -> bool {
    *threshold == DEFAULT_VISUAL_BLOCK_AT
}

#[allow(clippy::trivially_copy_pass_by_ref)]
pub(super) fn is_default_file_at(threshold: &VisualThreshold) -> bool {
    *threshold == DEFAULT_VISUAL_FILE_AT
}

/// What a confirmed finding does in `live` mode (§11.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualDisposition {
    /// At or above `block_at`: the code reviewer must fix or dispute it
    /// before the candidate lands.
    MustAddress,
    /// Below `block_at`, at or above `file_at`: an optional fix, filed
    /// after landing when the code reviewer does not fix it.
    Optional,
    /// Below `file_at`: recorded only.
    Recorded,
    /// At or above `block_at` in a run with no review before landing:
    /// nothing can resolve it, so it is filed as a `high` follow-up.
    FollowUpHigh,
}

impl VisualDisposition {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MustAddress => "must_address",
            Self::Optional => "optional",
            Self::Recorded => "recorded",
            Self::FollowUpHigh => "follow_up_high",
        }
    }
}

/// The disposition of a finding of verified `severity` under the
/// thresholds (§11.3). `review_layer` is whether a review runs before
/// landing that can resolve a blocking finding.
#[must_use]
pub fn visual_disposition(
    severity: &str,
    block_at: VisualThreshold,
    file_at: VisualThreshold,
    review_layer: bool,
) -> VisualDisposition {
    if block_at.admits(severity) {
        if review_layer {
            VisualDisposition::MustAddress
        } else {
            VisualDisposition::FollowUpHigh
        }
    } else if file_at.admits(severity) {
        VisualDisposition::Optional
    } else {
        VisualDisposition::Recorded
    }
}

/// How the code reviewer resolved a confirmed finding it was given (§1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualResolution {
    /// Its fix commit changes the cause.
    Fixed,
    /// Disputed with a reason; a blocking one holds for the operator.
    Disputed,
    /// A blocking dispute the operator accepted.
    DisputeAccepted,
    /// Left as it is (an optional finding the reviewer did not fix).
    Unfixed,
}

impl VisualResolution {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::Disputed => "disputed",
            Self::DisputeAccepted => "dispute_accepted",
            Self::Unfixed => "unfixed",
        }
    }

    /// Parse a reviewer's or a record's value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "fixed" => Some(Self::Fixed),
            "disputed" => Some(Self::Disputed),
            "dispute_accepted" => Some(Self::DisputeAccepted),
            "unfixed" => Some(Self::Unfixed),
            _ => None,
        }
    }
}

/// Which records one pass of the visual phase writes (§1, §12.2): the
/// delivery's capture of record under `visual/`, the recapture after a
/// review commit under `visual/recapture/`, and the post-landing
/// comparison under `visual/measure/`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualPass {
    #[default]
    Delivery,
    Recapture,
    Measure,
}

impl VisualPass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Delivery => "delivery",
            Self::Recapture => "recapture",
            Self::Measure => "measure",
        }
    }

    /// Parse a step input's `pass`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "delivery" => Some(Self::Delivery),
            "recapture" => Some(Self::Recapture),
            "measure" => Some(Self::Measure),
            _ => None,
        }
    }

    /// The artifact prefix of this pass, without a trailing slash.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Delivery => "visual",
            Self::Recapture => "visual/recapture",
            Self::Measure => "visual/measure",
        }
    }

    /// `<prefix>/capture.json`.
    #[must_use]
    pub fn capture_artifact(self) -> String {
        format!("{}/capture.json", self.prefix())
    }

    /// `<prefix>/checks.json`.
    #[must_use]
    pub fn checks_artifact(self) -> String {
        format!("{}/checks.json", self.prefix())
    }

    /// `<prefix>/settle.json`.
    #[must_use]
    pub fn settle_artifact(self) -> String {
        format!("{}/settle.json", self.prefix())
    }

    /// `<prefix>/review.json`.
    #[must_use]
    pub fn review_artifact(self) -> String {
        format!("{}/review.json", self.prefix())
    }

    /// `<prefix>/<mock-name>/<leaf>`: one captured image.
    #[must_use]
    pub fn image_artifact(self, mock_name: &str, leaf: &str) -> String {
        format!("{}/{mock_name}/{leaf}", self.prefix())
    }

    /// The purpose the pass's comparison review records.
    #[must_use]
    pub const fn purpose(self) -> &'static str {
        match self {
            Self::Measure => VISUAL_MEASURE_PURPOSE,
            Self::Delivery | Self::Recapture => VISUAL_REVIEW_PURPOSE,
        }
    }
}

/// An `[x, y, w, h]` box inside a `width`×`height` image.
#[must_use]
pub fn visual_region_inside(region: [u32; 4], width: u32, height: u32) -> bool {
    let [x, y, w, h] = region;
    w > 0
        && h > 0
        && u64::from(x) + u64::from(w) <= u64::from(width)
        && u64::from(y) + u64::from(h) <= u64::from(height)
}

/// Parse an image-region evidence location, `"<image>#<x>,<y>,<w>,<h>"`
/// (§11.2), the extension of finding verification's `path:line`.
#[must_use]
pub fn parse_visual_region_location(location: &str) -> Option<(String, [u32; 4])> {
    let (image, region) = location.trim().rsplit_once('#')?;
    if image.is_empty() || image.starts_with('/') || image.split('/').any(|part| part == "..") {
        return None;
    }
    let mut parts = region.split(',');
    let mut values = [0u32; 4];
    for value in &mut values {
        *value = parts.next()?.trim().parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some((image.to_string(), values))
}

/// One piece of image evidence a finding cites (§11.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualEvidence {
    /// The mock or its capture, as named in `capture.json`.
    pub image: String,
    /// `[x, y, w, h]` inside that image.
    pub region: [u32; 4],
    /// Optionally, the cause as `path:line` at the candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

/// The images a finding may cite for one delivery: every mock, capture
/// and side-by-side `capture.json` names, with the dimensions the
/// caller read from their stored bytes.
pub type VisualImageCatalog = BTreeMap<String, (u32, u32)>;

/// The catalog `record` admits, given the dimensions `dimensions`
/// reports for each artifact path it names (mocks included). A path
/// whose dimensions cannot be read is absent, so evidence naming it
/// is invalid rather than unchecked.
#[must_use]
pub fn visual_image_catalog(
    record: &VisualCaptureRecord,
    dimensions: &dyn Fn(&str) -> Option<(u32, u32)>,
) -> VisualImageCatalog {
    let mut catalog = VisualImageCatalog::new();
    for mock in &record.mocks {
        if let Some(size) = dimensions(&mock.artifact) {
            catalog.insert(mock.artifact.clone(), size);
        }
        if let Some(capture) = &mock.capture {
            for path in [&capture.path, &capture.side_by_side] {
                if let Some(size) = dimensions(path) {
                    catalog.insert(path.clone(), size);
                }
            }
        }
    }
    catalog
}

/// Validate one finding's evidence against this delivery's catalog
/// (§11.1): at least one cited image, every image in the catalog, and
/// every region inside its image. The error names the failure; the
/// caller drops the finding as `invalid_evidence` and counts it.
pub fn validate_visual_evidence(
    evidence: &[VisualEvidence],
    catalog: &VisualImageCatalog,
) -> Result<(), String> {
    if evidence.is_empty() {
        return Err("a finding must cite at least one image region".to_string());
    }
    for item in evidence {
        let Some((width, height)) = catalog.get(item.image.as_str()) else {
            return Err(format!(
                "evidence image `{}` is not in this delivery's capture",
                item.image
            ));
        };
        if !visual_region_inside(item.region, *width, *height) {
            return Err(format!(
                "evidence region {:?} is outside `{}` ({}x{})",
                item.region, item.image, width, height
            ));
        }
    }
    Ok(())
}

/// Validate one verifier evidence location (§11.2): an image region in
/// this delivery's catalog. `path:line` locations are finding
/// verification's own contract and are not judged here.
pub fn validate_visual_region_location(
    location: &str,
    catalog: &VisualImageCatalog,
) -> Result<(), String> {
    let Some((image, region)) = parse_visual_region_location(location) else {
        return Err(format!(
            "evidence location {location:?} is not `<image>#<x>,<y>,<w>,<h>`"
        ));
    };
    let Some((width, height)) = catalog.get(image.as_str()) else {
        return Err(format!(
            "evidence image `{image}` is not in this delivery's capture"
        ));
    };
    if !visual_region_inside(region, *width, *height) {
        return Err(format!(
            "evidence region {region:?} is outside `{image}` ({}x{})",
            width, height
        ));
    }
    Ok(())
}

/// A verifier's verdict on one finding (§11.2). `unverified` is the
/// recorded state of a finding no valid verdict decided: it never
/// blocks and is never filed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualVerdictKind {
    Confirmed,
    Rejected,
    Unverified,
}

impl VisualVerdictKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "CONFIRMED",
            Self::Rejected => "REJECTED",
            Self::Unverified => "unverified",
        }
    }
}

/// The deciding verdict recorded for one finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualFindingVerdict {
    pub verdict: VisualVerdictKind,
    /// The crew that decided it; never the reviewer's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    /// `high`, `medium` or `low`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    /// For CONFIRMED, the severity the verifier rated: the one
    /// recorded and scored, with `severity_source: verified` (§11.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    #[serde(default)]
    pub escalated: bool,
    pub reasoning: String,
    /// The verifier judged a declared deviation this finding touches
    /// and its basis did not hold (§2.5).
    #[serde(default)]
    pub deviation_rejected: bool,
    /// For a REJECTED verdict, the verifier's rejection ground
    /// (`premise_wrong`, `already_fixed`, `unreachable` or `duplicate`);
    /// only `premise_wrong` holds a deviation's basis (§2.5). Used during
    /// settlement but omitted from written records for older readers;
    /// records that already contain the ground can still be read.
    #[serde(default, skip_serializing)]
    pub rejection_reason: Option<String>,
}

/// One comparison or check finding in `visual/review.json` (§11.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualReviewFinding {
    pub finding_id: String,
    /// `comparison` (the reviewer) or `check` (§7).
    pub source: String,
    /// `1`–`8` (§8), or `rule:<id>` / `check:<name>`.
    pub question: String,
    pub mock: String,
    pub title: String,
    pub detail: String,
    #[serde(default)]
    pub evidence: Vec<VisualEvidence>,
    /// The reviewer's (or check's) severity.
    pub severity: String,
    /// The severity of record: the verifier's for a CONFIRMED finding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_severity: Option<String>,
    /// `verified` when `verified_severity` is the verifier's rating.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity_source: Option<String>,
    pub verdict: VisualFindingVerdict,
    /// In `live`, what a confirmed finding does under the thresholds
    /// (§11.3). Absent in `shadow` and on unconfirmed findings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<VisualDisposition>,
    /// How the code reviewer resolved it, once settled (§1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<VisualResolution>,
    /// The reviewer's dispute reason, or the operator's acceptance note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_reason: Option<String>,
    /// The recaptured images of a fixed finding's mock (§1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recaptured: Vec<String>,
    /// The follow-up task filed for it after landing (§11.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<String>,
}

impl VisualReviewFinding {
    /// The severity of record: the verifier's for a CONFIRMED finding,
    /// else the reviewer's or check's.
    #[must_use]
    pub fn severity_of_record(&self) -> &str {
        self.verified_severity.as_deref().unwrap_or(&self.severity)
    }

    /// Whether this finding is confirmed and must be resolved before the
    /// candidate lands.
    #[must_use]
    pub fn must_address(&self) -> bool {
        self.verdict.verdict == VisualVerdictKind::Confirmed
            && self.disposition == Some(VisualDisposition::MustAddress)
    }

    /// Whether this confirmed finding is filed as a follow-up after
    /// landing (§11.3): an optional fix left unfixed, a blocking dispute
    /// the operator accepted, or a blocking finding nothing could resolve.
    #[must_use]
    pub fn files_follow_up(&self) -> bool {
        if self.verdict.verdict != VisualVerdictKind::Confirmed || self.follow_up.is_some() {
            return false;
        }
        match self.disposition {
            Some(VisualDisposition::Optional) => self.resolution != Some(VisualResolution::Fixed),
            Some(VisualDisposition::MustAddress) => {
                self.resolution == Some(VisualResolution::DisputeAccepted)
            }
            Some(VisualDisposition::FollowUpHigh) => true,
            Some(VisualDisposition::Recorded) | None => false,
        }
    }
}

/// One declared deviation as the review recorded it (§2.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualDeviationRecord {
    pub mock: String,
    pub element: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<[u32; 4]>,
    pub reason: String,
    pub basis: String,
    /// `unjudged` (no finding touched it), `accepted` (a verifier held
    /// its basis) or `rejected` (a verifier rejected its basis).
    pub status: String,
}

/// The budgets one review ran under and what they admitted (§10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualReviewBudgetRecord {
    pub max_mocks: u32,
    pub max_images: u32,
    pub max_image_tokens: u64,
    pub max_findings: u32,
    /// Images staged for the reviewer, the canary included.
    pub images_shown: u32,
    pub estimated_image_tokens: u64,
    #[serde(default)]
    pub shown: Vec<VisualShownMock>,
    /// Mocks the image or token budget kept out, listed as not shown.
    #[serde(default)]
    pub not_shown: Vec<VisualNotShownMock>,
}

/// `visual/review.json`: the comparison review and its in-line
/// verification for one delivery (§8, §11). In shadow it is recorded
/// and scored under purpose `visual_review` only: nothing is filed,
/// nothing blocks and it is not given to the code reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualReviewRecord {
    pub schema_version: u32,
    /// Always `visual_review` ([`VISUAL_REVIEW_PURPOSE`], §11.4).
    pub purpose: String,
    pub mode: VisualReviewMode,
    pub task_id: String,
    pub candidate_sha: String,
    /// When settle recorded it (RFC 3339); scorecards window on it.
    #[serde(default)]
    pub recorded_at: String,
    /// `reviewed`, `not_established`, `no_stable_captures` or
    /// `reviewer_disabled`.
    pub outcome: String,
    #[serde(default)]
    pub not_established: Vec<VisualNotEstablished>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer_crew: Option<String>,
    #[serde(default)]
    pub verifier_crews: Vec<String>,
    pub budget: VisualReviewBudgetRecord,
    #[serde(default)]
    pub findings: Vec<VisualReviewFinding>,
    /// Findings dropped as `invalid_evidence` (§11.1), counted.
    pub invalid_evidence_dropped: u32,
    /// Findings dropped past `max_findings` (§10), counted.
    pub over_max_findings_dropped: u32,
    #[serde(default)]
    pub deviations: Vec<VisualDeviationRecord>,
    /// Shadow evidence (and the post-landing comparison) is recorded
    /// only; `false` when `live` gave findings dispositions (§11.3).
    pub recorded_only: bool,
    /// `visual_review.block_at` a live review ran under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_at: Option<VisualThreshold>,
    /// `visual_review.file_at` a live review ran under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_at: Option<VisualThreshold>,
    /// The recapture after a review commit, when one ran (§1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recapture: Option<VisualRecaptureRecord>,
}

/// The recapture of every mock's view at the review commit, with the
/// checks rerun (§1). No model round runs on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualRecaptureRecord {
    /// The review commit recaptured.
    pub candidate_sha: String,
    /// `passed`, `incomplete` (a blocking check finding is still present)
    /// or `not_established`.
    pub outcome: String,
    /// The finding ids of blocking check findings marked fixed that the
    /// recapture still finds.
    #[serde(default)]
    pub still_failing: Vec<String>,
}

impl VisualReviewRecord {
    /// Parse an attachment's bytes.
    pub fn parse(content: &[u8]) -> Result<Self, String> {
        let record: Self = serde_json::from_slice(content).map_err(|error| error.to_string())?;
        if record.schema_version != 1 {
            return Err(format!(
                "schema_version {} is not supported (expected 1)",
                record.schema_version
            ));
        }
        Ok(record)
    }

    /// Confirmed findings by disposition, for the live summary:
    /// must-address, optional and recorded (follow-up-high counts as
    /// must-address).
    #[must_use]
    pub fn disposition_counts(&self) -> (usize, usize, usize) {
        let mut counts = (0, 0, 0);
        for finding in &self.findings {
            match finding.disposition {
                Some(VisualDisposition::MustAddress | VisualDisposition::FollowUpHigh) => {
                    counts.0 += 1;
                }
                Some(VisualDisposition::Optional) => counts.1 += 1,
                Some(VisualDisposition::Recorded) => counts.2 += 1,
                None => {}
            }
        }
        counts
    }

    /// The confirmed findings still unresolved before landing: blocking
    /// ones not fixed and not accepted by the operator, and fixed blocking
    /// findings whose recapture failed, is absent, or still finds them (§1).
    #[must_use]
    pub fn unresolved_blocking(&self) -> Vec<&VisualReviewFinding> {
        self.findings
            .iter()
            .filter(|finding| finding.must_address())
            .filter(|finding| match finding.resolution {
                Some(VisualResolution::DisputeAccepted) => false,
                Some(VisualResolution::Fixed) => self.recapture.as_ref().is_none_or(|recapture| {
                    recapture.outcome != "passed"
                        || recapture.still_failing.contains(&finding.finding_id)
                }),
                _ => true,
            })
            .collect()
    }

    /// Findings counted by verdict, for the one-line summary.
    #[must_use]
    pub fn verdict_counts(&self) -> (usize, usize, usize) {
        let mut counts = (0, 0, 0);
        for finding in &self.findings {
            match finding.verdict.verdict {
                VisualVerdictKind::Confirmed => counts.0 += 1,
                VisualVerdictKind::Rejected => counts.1 += 1,
                VisualVerdictKind::Unverified => counts.2 += 1,
            }
        }
        counts
    }
}
