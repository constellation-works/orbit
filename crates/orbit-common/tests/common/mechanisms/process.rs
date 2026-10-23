//! Bounded child-process output capture.

use std::process::Command;
use std::time::Duration;

use orbit_common::process::output_capture::OUTPUT_TRUNCATED_MARKER;
use orbit_common::process::{
    run_bounded, run_bounded_capped, run_bounded_capped_complete_typed, run_bounded_capped_typed,
};

#[test]
fn bounded_run_drains_output_past_pipe_capacity_with_capped_retention() {
    // Natural EOF and prefix truncation must work for every bounded API.
    // 1 MiB per stream exceeds a pipe buffer, requiring continuous draining.
    for api in 0..4 {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "head -c 1048576 /dev/zero; head -c 1048576 /dev/zero >&2",
        ]);
        let limit = 4096;
        let deadline = Duration::from_secs(30);
        let output = match api {
            0 => run_bounded(&mut command, deadline).expect("uncapped run"),
            1 => run_bounded_capped(&mut command, deadline, limit).expect("capped run"),
            2 => run_bounded_capped_typed(&mut command, deadline, limit).expect("typed run"),
            _ => run_bounded_capped_complete_typed(&mut command, deadline, limit)
                .expect("EOF completion"),
        };
        assert!(output.status.success(), "API {api}: {:?}", output.status);
        for (label, stream) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
            let retained = if api == 0 { 1048576 } else { limit };
            let suffix = if api == 0 {
                &[][..]
            } else {
                OUTPUT_TRUNCATED_MARKER
            };
            assert_eq!(
                stream.len(),
                retained + suffix.len(),
                "API {api} {label} retention"
            );
            assert!(
                stream[..retained].iter().all(|byte| *byte == 0),
                "API {api} {label}"
            );
            assert!(stream.ends_with(suffix), "API {api} {label}");
        }
    }
}
