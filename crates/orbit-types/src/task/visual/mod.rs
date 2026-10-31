//! Visual review capture and comparison contracts.

mod budget;
mod capture;
mod capture_checks;
mod image;
mod review;

pub use budget::{
    VisualBudgetMock, VisualBudgetSelection, VisualNotShownMock, VisualShownForm, VisualShownMock,
    select_visual_budget,
};
pub use capture::{
    DEFAULT_VISUAL_MAX_MOCKS, VISUAL_ARTIFACT_PREFIX, VISUAL_CAPTURE_ARTIFACT,
    VISUAL_CAPTURE_MISMATCH, VISUAL_CHECKS_ARTIFACT, VISUAL_MAX_IMAGE_BYTES,
    VISUAL_SETTLE_ARTIFACT, VisualCaptureError, VisualCaptureImage, VisualCaptureOutcome,
    VisualCaptureRecord, VisualCapturedMock, VisualCheckFinding, VisualChecksRecord, VisualKitPin,
    VisualManifestCapture, VisualManifestRef, VisualMockDigest, VisualMockDigestRecord,
    VisualNotEstablished, VisualReviewContract, VisualReviewMode, VisualSettleOutcome,
    VisualSettleRecord, VisualSettleRefusal, is_visual_artifact,
};
pub use capture_checks::{
    VisualCaptureExpectation, visual_capture_mismatches, visual_review_summary,
    visual_review_summary_with_review,
};
pub use image::{visual_image_dimensions, visual_image_tokens};
pub use review::{
    DEFAULT_VISUAL_BLOCK_AT, DEFAULT_VISUAL_FILE_AT, DEFAULT_VISUAL_MAX_FINDINGS,
    DEFAULT_VISUAL_MAX_IMAGE_TOKENS, DEFAULT_VISUAL_MAX_IMAGES, VISUAL_DISPUTE_ACCEPTED_EVENT,
    VISUAL_IMAGE_TOKEN_CONSTANT, VISUAL_MEASURE_INVOCATION_IDS, VISUAL_MEASURE_PURPOSE,
    VISUAL_NO_VISION_CREW, VISUAL_REVIEW_ARTIFACT, VISUAL_REVIEW_PURPOSE, VISUAL_REVIEW_TAG,
    VISUAL_VERIFY_BATCH_SIZE, VisualDeviationRecord, VisualDisposition, VisualEvidence,
    VisualFindingVerdict, VisualImageCatalog, VisualPass, VisualRecaptureRecord, VisualResolution,
    VisualReviewBudgetRecord, VisualReviewFinding, VisualReviewRecord, VisualThreshold,
    VisualVerdictKind, parse_visual_region_location, validate_visual_evidence,
    validate_visual_region_location, visual_disposition, visual_image_catalog,
    visual_region_inside, visual_severity_rank, visual_severity_weight,
};
