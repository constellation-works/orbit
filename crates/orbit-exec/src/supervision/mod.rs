//! Exec's policy over the shared supervisor in
//! [`orbit_common::process::supervisor`]: fail-at-limit capture of both
//! streams (with a redacting echo in debug mode) or a stdout relay, a
//! five-second termination grace period, signal interception, a one-second
//! drain budget and the stopped-descendant watch. Supervision diagnostics are
//! appended to the captured stderr.

use std::io::PipeWriter;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_common::process::output_capture::capture_limit_from_env;
use orbit_common::process::stopped_descendants::stopped_descendant_threshold;
use orbit_common::process::supervisor::{
    CaptureSink, Outcome, OutputSink, RelaySink, Stdin, Stream, SupervisedChild, Supervision,
    Termination,
};

pub(crate) const TERMINATION_GRACE_PERIOD: Duration = Duration::from_secs(5);
pub use orbit_common::process::supervisor::{
    INTERRUPTED_SIGNAL_NOTE, OUTPUT_CAPTURE_LIMIT_NOTE, ProcessStopCause, TERMINATION_STATUS_NOTE,
};

const ORBIT_EXEC_OUTPUT_CAPTURE_LIMIT_ENV: &str = "ORBIT_EXEC_OUTPUT_CAPTURE_LIMIT_BYTES";
const DEFAULT_OUTPUT_CAPTURE_LIMIT_BYTES: usize = 1024 * 1024;

/// How long pipe workers may keep running after the supervised child is gone,
/// when a descendant outside its process group still holds a pipe.
const DRAIN_BUDGET: Duration = Duration::from_secs(1);

/// What one exec run asks of the supervisor.
pub(crate) struct ExecSupervision<'a> {
    pub(crate) timeout_ms: Option<u64>,
    /// Echo both streams, redacted, to Orbit's stderr.
    pub(crate) debug: bool,
    pub(crate) stdin_payload: Option<Vec<u8>>,
    /// Forward stdout here instead of capturing it.
    pub(crate) stdout_relay: Option<PipeWriter>,
    pub(crate) cancelled: Option<&'a AtomicBool>,
    /// Kill the group at once instead of signalling it and allowing the
    /// termination grace period.
    pub(crate) kill_at_once: bool,
}

/// Supervise `child` under exec's policy. The outcome's stderr carries the
/// supervision diagnostics after the child's own output.
pub(crate) fn supervise(
    child: SupervisedChild,
    run: ExecSupervision<'_>,
) -> Result<Outcome, OrbitError> {
    let limit = capture_limit_from_env(
        ORBIT_EXEC_OUTPUT_CAPTURE_LIMIT_ENV,
        DEFAULT_OUTPUT_CAPTURE_LIMIT_BYTES,
    );
    let stdout: Box<dyn OutputSink> = match run.stdout_relay {
        Some(relay) => Box::new(RelaySink::new(relay)),
        None => Box::new(CaptureSink::new("stdout", limit).with_redacting_echo(run.debug)),
    };
    let mut outcome = child.supervise(Supervision {
        stdin: run.stdin_payload.map_or(Stdin::Null, Stdin::Payload),
        stdout: Stream::Sink(stdout),
        stderr: Stream::Sink(Box::new(
            CaptureSink::new("stderr", limit).with_redacting_echo(run.debug),
        )),
        timeout: run.timeout_ms.map(Duration::from_millis),
        extendable_deadline: None,
        clock: None,
        termination: if run.kill_at_once {
            Termination::KillNow
        } else {
            Termination::Grace(TERMINATION_GRACE_PERIOD)
        },
        cancelled: run.cancelled,
        intercept_signals: true,
        drain_budget: DRAIN_BUDGET,
        cap_drain_at_deadline: false,
        require_eof: false,
        stopped_descendants: Some(stopped_descendant_threshold()),
        on_stopped_descendant: None,
        on_spawn: None,
        every: None,
    })?;
    append_diagnostics(&mut outcome);
    Ok(outcome)
}

/// Explain on stderr what supervision observed, so a caller reading only the
/// text never mistakes a stopped or cut run for the child's own failure.
fn append_diagnostics(outcome: &mut Outcome) {
    let mut notes = Vec::new();
    if outcome.timed_out {
        notes.push("process timed out".to_string());
    }
    notes.extend(outcome.stop_causes.iter().map(ToString::to_string));
    notes.extend(
        outcome
            .stopped_descendants
            .iter()
            .map(|stopped| format!("supervisor: {}", stopped.describe())),
    );
    // Output after this point was discarded; without the note a caller could
    // mistake a cut stream for the whole of it.
    if outcome.drain_stopped {
        notes.push(format!(
            "process pipes were still held open outside its process group; \
             stopped draining {} ms after it ended",
            DRAIN_BUDGET.as_millis()
        ));
    }
    for note in notes {
        if !outcome.stderr.is_empty() {
            outcome.stderr.push(b'\n');
        }
        outcome.stderr.extend_from_slice(note.as_bytes());
    }
}
