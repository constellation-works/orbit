#![allow(missing_docs)]

/// [ORB-10879] The composition rule behind the exit-0 attribution, asserted
/// without Bubblewrap: the spawn-boundary tests in `tests/orchestrator.rs` are
/// `#[ignore]`d because a host whose kernel forbids unprivileged user
/// namespaces (including every nested Orbit sandbox) cannot run them, so the
/// message contract gets a check that always runs.
#[test]
fn sandbox_write_attribution_rides_along_with_the_frame_classification() {
    let denial = "Orbit linux-bwrap policy denied the attempted write: \
                  `/w/.orbit/x` is not writable inside the sandbox: denyModify rule `!/w/.orbit/**` shadows it";

    let composed = super::super::response_diagnostics::with_sandbox_write_attribution(
        "agent step did not complete".to_string(),
        Some(denial),
    );
    assert!(
        composed.starts_with("agent step did not complete"),
        "the frame classification stays the head of the message: {composed}"
    );
    assert!(
        composed.ends_with(denial),
        "the denial text is appended verbatim, not reformatted: {composed}"
    );

    assert_eq!(
        super::super::response_diagnostics::with_sandbox_write_attribution(
            "agent step did not complete".to_string(),
            None,
        ),
        "agent step did not complete",
        "a step that hit no write denial keeps its message unchanged"
    );
}
