//! A closed stdout is a normal way for a command to end, not a failure.
//!
//! `orbit task list | head -1` closes the read end after one line. The next
//! `println!` fails with `EPIPE`, and `std::io::_print` responds by panicking —
//! so the user's terminal fills with a backtrace for having typed `head`.
//! `docs/design/terminal-interface/specs/output-modes.md` §5 says instead:
//! exit `0`, silently.
//!
//! Two paths reach that outcome, because the crate writes stdout two ways.
//! [`install_handler`] covers the `println!` family, whose failure mode is a
//! panic. [`is_broken_pipe`] covers the handful of call sites that hold a
//! locked writer and get an `io::Error` back instead.

use std::io;

/// Exit `0` instead of panicking when a `println!` fails on a closed stdout.
///
/// Installed once, from `main`, before any output. Every other panic reaches
/// the hook this one replaced, so a genuine bug still reports normally.
///
/// The alternative — restoring `SIGPIPE` to `SIG_DFL` so the process dies of
/// the signal — is also silent, but exits `141`. The spec asks for `0`: a
/// consumer that read what it wanted and closed the pipe got what it asked
/// for, and `set -o pipefail` should not turn that into a failed script.
pub fn install_handler() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if panicked_on_a_closed_stdout(info) {
            std::process::exit(0);
        }
        previous(info);
    }));
}

/// Whether an `io::Error` is the read end of stdout having gone away.
pub fn is_broken_pipe(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::BrokenPipe
}

fn panicked_on_a_closed_stdout(info: &std::panic::PanicHookInfo<'_>) -> bool {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or_default();
    is_closed_stdout_panic(message)
}

/// Recognize the panic `std::io::_print` raises when stdout is closed.
///
/// The payload is the formatted string `failed printing to stdout: <error>`,
/// and the error's `Display` ends in `(os error 32)` — `EPIPE` on every unix.
/// Matching the prefix alone would also swallow a full disk, which is a real
/// failure and must keep panicking, so both halves have to agree.
// pub(super) widened for sibling-layout tests in output/tests/pipe.rs
pub(super) fn is_closed_stdout_panic(message: &str) -> bool {
    message.starts_with("failed printing to stdout")
        && (message.contains("os error 32") || message.contains("Broken pipe"))
}
