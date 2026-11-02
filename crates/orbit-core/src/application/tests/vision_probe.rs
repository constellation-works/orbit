//! The probe report records a finished run from that run's canary check.
//! A miss stays `image_not_seen` when the probe run itself succeeded, and a
//! returned code stays `seen`. One crew's result does not rewrite the other.

use super::super::job::pipeline::PipelineWaitStatus;
use super::super::vision_probe::{VisionProbeResult, classify};

#[test]
fn successful_run_without_the_code_is_image_not_seen_and_a_returned_code_is_seen() {
    let (miss, miss_detail) = classify(&PipelineWaitStatus::Success, None, Some(false));
    assert_eq!(miss, VisionProbeResult::ImageNotSeen);
    assert_eq!(
        miss_detail.as_deref(),
        Some("finished without returning the canary code"),
        "a completed probe that did not return the code is image_not_seen"
    );

    let (seen, seen_detail) = classify(&PipelineWaitStatus::Success, None, Some(true));
    assert_eq!(seen, VisionProbeResult::Seen);
    assert!(
        seen_detail.is_none(),
        "a returned canary code is seen, got {seen_detail:?}"
    );
    assert_ne!(
        miss, seen,
        "one crew's miss must not replace another crew's seen result"
    );

    let (marked, _) = classify(
        &PipelineWaitStatus::Failed,
        Some(
            "[image_not_seen] provider `codex` finished without returning the canary code in \
             `result.canary` (it returned a different code)",
        ),
        None,
    );
    assert_eq!(marked, VisionProbeResult::ImageNotSeen);

    let (failed, _) = classify(
        &PipelineWaitStatus::Failed,
        Some("provider exited before the canary check"),
        Some(true),
    );
    assert_eq!(
        failed,
        VisionProbeResult::Failed,
        "a run that failed after the code was returned stays failed"
    );
}
