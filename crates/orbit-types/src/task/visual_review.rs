//! Visual review's comparison findings and their in-line verification
//! (visual-review design §8, §10, §11): the system-written
//! `visual/review.json`, image-region evidence and its validation, image
//! dimensions and per-provider token estimates, and the budget selection
//! that decides which mocks one review invocation is shown.
//!
//! Everything here is pure: callers supply image bytes, the capture of
//! record and the budgets, and every refusal is named. In this phase the
//! record is shadow evidence only — nothing here files, blocks or feeds
//! the code reviewer.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{VisualCaptureRecord, VisualNotEstablished, VisualReviewMode};

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

/// The pixel dimensions of a PNG, JPEG, WebP or GIF image, read from its
/// header. `None` for any other bytes: an image whose size cannot be
/// read cannot be budgeted or cited by region.
#[must_use]
pub fn visual_image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        // IHDR: width and height are the first two big-endian u32s.
        let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
        let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
        return nonzero(width, height);
    }
    if bytes.starts_with(b"GIF8") {
        let width = u32::from(u16::from_le_bytes(bytes.get(6..8)?.try_into().ok()?));
        let height = u32::from(u16::from_le_bytes(bytes.get(8..10)?.try_into().ok()?));
        return nonzero(width, height);
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        return jpeg_dimensions(bytes);
    }
    if bytes.get(0..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return webp_dimensions(bytes);
    }
    None
}

fn nonzero(width: u32, height: u32) -> Option<(u32, u32)> {
    (width > 0 && height > 0).then_some((width, height))
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut offset = 2usize;
    while offset + 4 <= bytes.len() {
        if bytes[offset] != 0xff {
            return None;
        }
        let marker = bytes[offset + 1];
        // Start-of-frame markers carry precision, height, width.
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            let height = u32::from(u16::from_be_bytes(
                bytes.get(offset + 5..offset + 7)?.try_into().ok()?,
            ));
            let width = u32::from(u16::from_be_bytes(
                bytes.get(offset + 7..offset + 9)?.try_into().ok()?,
            ));
            return nonzero(width, height);
        }
        if marker == 0xda {
            return None;
        }
        let length = usize::from(u16::from_be_bytes(
            bytes.get(offset + 2..offset + 4)?.try_into().ok()?,
        ));
        if length < 2 {
            return None;
        }
        offset += 2 + length;
    }
    None
}

fn webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    match bytes.get(12..16)? {
        b"VP8X" => {
            let width =
                1 + u32::from_le_bytes([*bytes.get(24)?, *bytes.get(25)?, *bytes.get(26)?, 0]);
            let height =
                1 + u32::from_le_bytes([*bytes.get(27)?, *bytes.get(28)?, *bytes.get(29)?, 0]);
            nonzero(width, height)
        }
        b"VP8L" => {
            if *bytes.get(20)? != 0x2f {
                return None;
            }
            let bits = u32::from_le_bytes(bytes.get(21..25)?.try_into().ok()?);
            nonzero((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
        }
        b"VP8 " => {
            // Lossy bitstream: 14-bit dimensions after the frame tag and
            // the 9d 01 2a start code, each with two scale bits on top.
            if bytes.get(23..26)? != [0x9d, 0x01, 0x2a] {
                return None;
            }
            let width = u32::from(u16::from_le_bytes(bytes.get(26..28)?.try_into().ok()?)) & 0x3fff;
            let height =
                u32::from(u16::from_le_bytes(bytes.get(28..30)?.try_into().ok()?)) & 0x3fff;
            nonzero(width, height)
        }
        _ => None,
    }
}

/// The estimated image tokens of one `width`×`height` image for
/// `provider` (§10). Claude models cost roughly width × height / 750;
/// a provider without a published rule uses
/// [`VISUAL_IMAGE_TOKEN_CONSTANT`] per image. The estimate decides
/// inclusion only; measured tokens are what an experiment reads.
#[must_use]
pub fn visual_image_tokens(provider: &str, width: u32, height: u32) -> u64 {
    let provider = provider.to_ascii_lowercase();
    if provider.contains("claude") || provider.contains("anthropic") {
        let pixels = u64::from(width) * u64::from(height);
        pixels.div_ceil(750).max(1)
    } else {
        VISUAL_IMAGE_TOKEN_CONSTANT
    }
}

/// One mock's images as the budget sees them: the mock, its capture and
/// the side-by-side, each with its dimensions when they could be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualBudgetMock {
    pub mock: String,
    pub mock_dimensions: Option<(u32, u32)>,
    pub capture_dimensions: Option<(u32, u32)>,
    pub side_by_side_dimensions: Option<(u32, u32)>,
}

/// How one shown mock reaches the reviewer (§10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualShownForm {
    /// Mock and capture as two images.
    Pair,
    /// Only the side-by-side, because both images would not fit.
    SideBySide,
}

/// One mock the review invocation is shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualShownMock {
    pub mock: String,
    pub form: VisualShownForm,
    pub images: u32,
    pub estimated_image_tokens: u64,
}

/// One mock the budget kept out of the invocation, with why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualNotShownMock {
    pub mock: String,
    /// `max_images`, `max_image_tokens` or `unreadable_image`.
    pub reason: String,
}

/// The budget selection for one invocation (§10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualBudgetSelection {
    pub shown: Vec<VisualShownMock>,
    pub not_shown: Vec<VisualNotShownMock>,
    /// Images staged, the canary included.
    pub images: u32,
    pub estimated_image_tokens: u64,
}

/// Choose which of `mocks` (already in §10 order: acceptance-criteria
/// mocks first, then artifact name) one invocation is shown, checked
/// before dispatch. Each mock costs two images (mock and capture); the
/// side-by-side is sent only when both would not fit. The canary is
/// first, so one image of `max_images` is reserved for it. What does
/// not fit is listed as not shown, never silently dropped.
#[must_use]
pub fn select_visual_budget(
    mocks: &[VisualBudgetMock],
    provider: &str,
    max_images: u32,
    max_image_tokens: u64,
) -> VisualBudgetSelection {
    let mut selection = VisualBudgetSelection {
        shown: Vec::new(),
        not_shown: Vec::new(),
        images: 1, // the canary
        estimated_image_tokens: 0,
    };
    let fits = |selection: &VisualBudgetSelection, images: u32, tokens: u64| {
        selection.images + images <= max_images
            && selection.estimated_image_tokens + tokens <= max_image_tokens
    };
    for mock in mocks {
        let tokens =
            |dimensions: (u32, u32)| visual_image_tokens(provider, dimensions.0, dimensions.1);
        let pair = mock
            .mock_dimensions
            .zip(mock.capture_dimensions)
            .map(|(mock_dims, capture_dims)| tokens(mock_dims) + tokens(capture_dims));
        let side = mock.side_by_side_dimensions.map(tokens);
        if let Some(pair_tokens) = pair
            && fits(&selection, 2, pair_tokens)
        {
            selection.images += 2;
            selection.estimated_image_tokens += pair_tokens;
            selection.shown.push(VisualShownMock {
                mock: mock.mock.clone(),
                form: VisualShownForm::Pair,
                images: 2,
                estimated_image_tokens: pair_tokens,
            });
            continue;
        }
        if let Some(side_tokens) = side
            && fits(&selection, 1, side_tokens)
        {
            selection.images += 1;
            selection.estimated_image_tokens += side_tokens;
            selection.shown.push(VisualShownMock {
                mock: mock.mock.clone(),
                form: VisualShownForm::SideBySide,
                images: 1,
                estimated_image_tokens: side_tokens,
            });
            continue;
        }
        let reason = if pair.is_none() && side.is_none() {
            "unreadable_image"
        } else if selection.images + 1 > max_images {
            "max_images"
        } else {
            "max_image_tokens"
        };
        selection.not_shown.push(VisualNotShownMock {
            mock: mock.mock.clone(),
            reason: reason.to_string(),
        });
    }
    selection
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
    /// only `premise_wrong` holds a deviation's basis (§2.5). Absent on
    /// records written before the ground was carried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    /// This record is shadow evidence: recorded only, until a later
    /// phase makes a mode live.
    pub recorded_only: bool,
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
