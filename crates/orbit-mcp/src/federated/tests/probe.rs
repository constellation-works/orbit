//! Delivery budget and post-dispatch classification.
//!
//! The fake destination here is a child process that holds the pipe open and
//! never writes a line, so a real [`DestinationSession`] runs its own timeout
//! path without an SSH host: everything the mux sends is accepted and nothing
//! is ever answered.

use super::super::probe::DestinationSession;
use super::fixtures::{OWNER_MACHINE, destination};
use orbit_common::OrbitError;

use serde_json::json;

use std::process::{Command, Stdio};

use std::time::{Duration, Instant};

/// A destination that takes the request and stops there.
fn stalled_session(budget: Duration) -> DestinationSession {
    let child = Command::new("sleep")
        .arg("30")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a stalled destination");
    DestinationSession::start(destination("orbit-owner", OWNER_MACHINE), child, budget)
        .expect("start a session against the stalled destination")
}

/// A destination that streams garbage as fast as the pipe allows. The reader
/// queue applies backpressure instead of buffering the flood while the
/// consumer decides, and the first non-JSON line ends the session promptly.
#[cfg(unix)]
#[test]
fn a_flooding_destination_is_refused_without_buffering_the_flood() {
    let child = Command::new("yes")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a flooding destination");
    let mut session = DestinationSession::start(
        destination("orbit-owner", OWNER_MACHINE),
        child,
        Duration::from_secs(5),
    )
    .expect("start a session against the flooding destination");

    let started = Instant::now();
    let error = session
        .handshake()
        .expect_err("a stream of `y` lines is not an MCP answer");
    assert!(
        matches!(error, OrbitError::UnreachableDestination(_)),
        "{error}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the first bad line must end the session, not the deadline"
    );
}

#[test]
fn a_lost_internal_admission_answer_is_outcome_unknown() {
    let mut session = stalled_session(Duration::from_millis(50));
    let error = session
        .call_internal_drain(
            "orbit.task.pull",
            json!({"request_id":"original-admission"}),
        )
        .expect_err("no reply");
    assert!(
        matches!(error, OrbitError::OutcomeUnknown { .. }),
        "a sent internal admission may have committed: {error}"
    );
}
