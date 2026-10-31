//! Visual review's capture of record (visual-review design §4.4, §4.5,
//! §6, §7): the reserved `visual/` artifact prefix, the contract a
//! delivery captures against, the system-written `visual/capture.json`,
//! `visual/checks.json` and `visual/settle.json` records, the owner's
//! check of a claimed leaf's returned captures, and the one-line summary
//! run show, task show and the PR body print.
//!
//! Everything here is pure: callers supply artifact digests and the
//! expected candidate, and every mismatch is named.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::super::{MockDigest, has_verify_mock_ui_tag};
use super::review::{
    DEFAULT_VISUAL_MAX_FINDINGS, DEFAULT_VISUAL_MAX_IMAGE_TOKENS, DEFAULT_VISUAL_MAX_IMAGES,
    VisualThreshold, default_block_at, default_file_at, is_default_block_at, is_default_file_at,
};

fn default_visual_reviewer() -> bool {
    true
}

fn default_visual_max_images() -> u32 {
    DEFAULT_VISUAL_MAX_IMAGES
}

fn default_visual_max_image_tokens() -> u64 {
    DEFAULT_VISUAL_MAX_IMAGE_TOKENS
}

fn default_visual_max_findings() -> u32 {
    DEFAULT_VISUAL_MAX_FINDINGS
}

/// Prefix reserved for Orbit's system writer (§4.5): an agent or operator
/// write under it is refused.
pub const VISUAL_ARTIFACT_PREFIX: &str = "visual/";
/// The capture of record (§4.4).
pub const VISUAL_CAPTURE_ARTIFACT: &str = "visual/capture.json";
/// The deterministic checks' findings for one capture (§7).
pub const VISUAL_CHECKS_ARTIFACT: &str = "visual/checks.json";
/// The settle step's verdict on the capture (§4.4).
pub const VISUAL_SETTLE_ARTIFACT: &str = "visual/settle.json";
/// The owner's refusal code for a claimed handoff whose returned captures
/// do not match its captured contract (§6.2).
pub const VISUAL_CAPTURE_MISMATCH: &str = "visual_capture_mismatch";
/// Largest image the capture attaches (the task artifact limit, §4.5).
pub const VISUAL_MAX_IMAGE_BYTES: usize = 1024 * 1024;
/// Mocks compared per delivery when `visual_review.max_mocks` is unset.
pub const DEFAULT_VISUAL_MAX_MOCKS: u32 = 12;

/// Whether a canonical artifact path lies under `visual/` (any ASCII case,
/// so a case-insensitive owner cannot be bypassed with `Visual/`).
#[must_use]
pub fn is_visual_artifact(path: &str) -> bool {
    path.split_once('/')
        .is_some_and(|(first, _)| first.eq_ignore_ascii_case("visual"))
}

/// `visual_review.mode` (scope › Configuration).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualReviewMode {
    /// No capture, checks or review in delivery.
    #[default]
    Off,
    /// Capture and check, record everything, block and file nothing.
    Shadow,
    /// Capture, check and compare; confirmed findings block, are given
    /// to the code reviewer and are filed under `block_at`/`file_at`.
    Live,
}

impl VisualReviewMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Shadow => "shadow",
            Self::Live => "live",
        }
    }

    /// Parse a configured value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "off" => Some(Self::Off),
            "shadow" => Some(Self::Shadow),
            "live" => Some(Self::Live),
            _ => None,
        }
    }
}

/// One view manifest as a delivery read it at its base revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualManifestRef {
    pub path: String,
    /// The base revision the bytes were read at.
    pub revision: String,
    pub sha256: String,
}

/// A manifest with its bytes, as a claim's contract carries it (§6.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualManifestCapture {
    pub reference: VisualManifestRef,
    /// The manifest's UTF-8 text at `revision`.
    pub content: String,
}

/// One mock digest frozen at admission, in the `visual/mock-digests.json`
/// shape ([`super::super::MockDigestRecord`]) without its flattening, so a
/// contract round-trips through strict decoding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualMockDigest {
    pub artifact: String,
    pub view: String,
    pub state: String,
    pub theme: String,
    pub width: u32,
    pub sha256: String,
}

impl From<&MockDigest> for VisualMockDigest {
    fn from(digest: &MockDigest) -> Self {
        Self {
            artifact: digest.mock.artifact.clone(),
            view: digest.mock.view.clone(),
            state: digest.mock.state.clone(),
            theme: digest.mock.theme.clone(),
            width: digest.mock.width,
            sha256: digest.sha256.clone(),
        }
    }
}

/// `visual/mock-digests.json` read back: the frozen digests (§2.4).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct VisualMockDigestRecord {
    pub schema_version: u32,
    pub mocks: Vec<VisualMockDigest>,
}

/// What one delivery captures against, read by the owner when the delivery
/// is admitted (§2.4, §6.2): the `visual_review` settings, the manifests at
/// the base revision and the mocks' digests. A claimed leaf runs this
/// contract, never its replica's settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualReviewContract {
    pub task_id: String,
    pub mode: VisualReviewMode,
    pub default_width: u32,
    pub max_mocks: u32,
    /// `visual_review.reviewer`: run the comparison reviewer and its
    /// verifier. Contracts captured before the reviewer existed run it.
    #[serde(default = "default_visual_reviewer")]
    pub reviewer: bool,
    /// `visual_review.max_images`: images per review invocation.
    #[serde(default = "default_visual_max_images")]
    pub max_images: u32,
    /// `visual_review.max_image_tokens`: estimated image tokens per
    /// review invocation.
    #[serde(default = "default_visual_max_image_tokens")]
    pub max_image_tokens: u64,
    /// `visual_review.max_findings`: findings accepted from one review.
    #[serde(default = "default_visual_max_findings")]
    pub max_findings: u32,
    pub require_established: bool,
    pub manifests: Vec<VisualManifestCapture>,
    /// Mock digests frozen at admission, in artifact order.
    pub mocks: Vec<VisualMockDigest>,
    /// `visual_review.block_at`; omitted at its default so older readers
    /// decode the contract.
    #[serde(
        default = "default_block_at",
        skip_serializing_if = "is_default_block_at"
    )]
    pub block_at: VisualThreshold,
    /// `visual_review.file_at`; omitted at its default.
    #[serde(
        default = "default_file_at",
        skip_serializing_if = "is_default_file_at"
    )]
    pub file_at: VisualThreshold,
}

impl VisualReviewContract {
    /// Whether delivery runs the visual phase for a task with `tags` under
    /// `mode` (§1): only `verify-mock-ui` tasks, only when mode is not off.
    #[must_use]
    pub fn applies_to(tags: &[String], mode: VisualReviewMode) -> bool {
        mode != VisualReviewMode::Off && has_verify_mock_ui_tag(tags)
    }

    /// The mocks this delivery captures, at most `max_mocks`, and the
    /// artifact names left over budget (§10: by artifact name).
    #[must_use]
    pub fn budgeted_mocks(&self) -> (Vec<&VisualMockDigest>, Vec<String>) {
        let mut mocks: Vec<&VisualMockDigest> = self.mocks.iter().collect();
        mocks.sort_by(|left, right| left.artifact.cmp(&right.artifact));
        let limit = usize::try_from(self.max_mocks).unwrap_or(usize::MAX);
        let over = mocks
            .iter()
            .skip(limit)
            .map(|mock| mock.artifact.clone())
            .collect();
        mocks.truncate(limit);
        (mocks, over)
    }
}

/// `outcome` of a capture (§4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualCaptureOutcome {
    Captured,
    NotEstablished,
}

/// Why a capture is not established (§6.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualNotEstablished {
    /// `browser_kit_missing`, `kit_pin_mismatch`, `setup_failed`,
    /// `no_vision_crew` or `capture_failed`.
    pub reason: String,
    pub detail: String,
}

/// The browser kit a capture ran with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualKitPin {
    pub playwright: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pixel_helper: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<String>,
}

/// One mock's capture of record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualCaptureImage {
    pub path: String,
    pub side_by_side: String,
    pub side_by_side_sha256: String,
    /// The commit the capture shows.
    pub commit: String,
    pub viewport: [u32; 2],
    pub device_scale_factor: u32,
    pub fixture: String,
    /// SHA-256 of the resolved setup command and the candidate tree it ran
    /// in, which pins every fixture seed the setup reads.
    pub fixture_sha256: String,
    pub captured_at: String,
    pub sha256: String,
    /// `stable`, `stable_after_retry` or `unstable` (§4.3).
    pub stability: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_region: Option<[u32; 4]>,
    /// `png` or `webp`.
    pub encoding: String,
    pub lossy: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub off_loopback_requests: Vec<String>,
    /// For a `color_scheme_invariant` app: whether the opposite colour
    /// preference rendered differently (§7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_scheme_leak: Option<bool>,
}

/// Why one mock produced no capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualCaptureError {
    /// `setup_failed`, `render_timeout` or `image_over_budget`.
    pub code: String,
    pub message: String,
}

/// One mock in `visual/capture.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualCapturedMock {
    pub artifact: String,
    /// Digest of the mock bytes compared, as captured.
    pub sha256: String,
    pub view: String,
    pub state: String,
    pub theme: String,
    pub width: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<VisualCaptureImage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<VisualCaptureError>,
}

/// `visual/capture.json`, the evidence of record (§4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualCaptureRecord {
    pub schema_version: u32,
    pub outcome: VisualCaptureOutcome,
    pub mode: VisualReviewMode,
    pub task_id: String,
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kit: Option<VisualKitPin>,
    pub candidate_sha: String,
    pub manifests: Vec<VisualManifestRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frozen_instant: Option<String>,
    pub mocks: Vec<VisualCapturedMock>,
    #[serde(default)]
    pub over_budget: Vec<String>,
    #[serde(default)]
    pub not_established: Vec<VisualNotEstablished>,
}

impl VisualCaptureRecord {
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

    /// Every image artifact the record names, with its recorded digest.
    #[must_use]
    pub fn images(&self) -> Vec<(&str, &str)> {
        self.mocks
            .iter()
            .filter_map(|mock| mock.capture.as_ref())
            .flat_map(|capture| {
                [
                    (capture.path.as_str(), capture.sha256.as_str()),
                    (
                        capture.side_by_side.as_str(),
                        capture.side_by_side_sha256.as_str(),
                    ),
                ]
            })
            .collect()
    }
}

/// One finding of the deterministic checks (§7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualCheckFinding {
    /// `rule:<id>` for a UI rule, `check:<name>` for a capture check.
    pub check: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mock: Option<String>,
    pub severity: String,
    pub title: String,
    pub detail: String,
    /// `change` or `manifest` (an `unstable_view` is the manifest's).
    pub against: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<[u32; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurements: Option<serde_json::Value>,
}

/// `visual/checks.json`: what the checks found on one capture. In shadow
/// nothing here blocks or is filed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualChecksRecord {
    pub schema_version: u32,
    pub mode: VisualReviewMode,
    pub candidate_sha: String,
    /// Whether any finding blocks delivery: always false before phase 6.
    pub blocking: bool,
    pub findings: Vec<VisualCheckFinding>,
    /// Rule results that passed, errored or were skipped, by status.
    #[serde(default)]
    pub rule_results: BTreeMap<String, u64>,
    /// Why no UI rule ran, when none did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules_unavailable: Option<String>,
}

impl VisualChecksRecord {
    /// Findings counted by severity, highest first.
    #[must_use]
    pub fn severity_counts(&self) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<u8, (String, usize)> = BTreeMap::new();
        for finding in &self.findings {
            let rank = severity_rank(&finding.severity);
            counts
                .entry(rank)
                .or_insert_with(|| (finding.severity.clone(), 0))
                .1 += 1;
        }
        counts.into_values().collect()
    }
}

fn severity_rank(severity: &str) -> u8 {
    match severity {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        "nit" => 4,
        _ => 5,
    }
}

/// A refusal the settle step records (§4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualSettleRefusal {
    /// `stale_capture` or `mock_changed`.
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mock: Option<String>,
    pub detail: String,
}

/// `outcome` of a settled capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualSettleOutcome {
    /// Captured on this candidate against the frozen mocks.
    Established,
    /// The capture recorded `not_established`.
    NotEstablished,
    /// The capture is refused (`stale_capture`, `mock_changed`).
    Refused,
}

/// `visual/settle.json`: the settle step's verdict on the capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualSettleRecord {
    pub schema_version: u32,
    pub candidate_sha: String,
    pub outcome: VisualSettleOutcome,
    #[serde(default)]
    pub refusals: Vec<VisualSettleRefusal>,
    #[serde(default)]
    pub not_established: Vec<VisualNotEstablished>,
    /// Whether `visual_review.require_established` held the candidate.
    pub held: bool,
}
