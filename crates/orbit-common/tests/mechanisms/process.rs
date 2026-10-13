//! Bounded child-process output capture.

use std::process::Command;
use std::time::Duration;

use orbit_common::process::output_capture::OUTPUT_TRUNCATED_MARKER;
use orbit_common::process::run_bounded_capped;

#[test]
fn bounded_run_drains_output_past_pipe_capacity_with_capped_retention() {
    // 1 MiB per stream is far past any pipe buffer: an undrained child would
    // block on write and hit the deadline instead of exiting.
    let mut command = Command::new("sh");
    command.args([
        "-c",
        "head -c 1048576 /dev/zero; head -c 1048576 /dev/zero >&2; echo done >&2",
    ]);
    let limit = 4096;
    let output =
        run_bounded_capped(&mut command, Duration::from_secs(30), limit).expect("drained run");
    assert!(output.status.success(), "{:?}", output.status);
    for (label, stream) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
        assert_eq!(
            stream.len(),
            limit + OUTPUT_TRUNCATED_MARKER.len(),
            "{label} retention"
        );
        assert!(stream[..limit].iter().all(|byte| *byte == 0), "{label}");
        assert!(stream.ends_with(OUTPUT_TRUNCATED_MARKER), "{label}");
    }
}
