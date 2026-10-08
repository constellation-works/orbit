//! Failed-step log classification and the normalized error signature: the
//! root-cause identity CI collection compares to decide whether a red run
//! reproduced, and CI failure filing keys its dedupe on.
//!
//! One owner, so a reproduction is judged by exactly the identity a filed
//! task carries.

use orbit_tools::github_cli::strip_ansi_sequences;

/// Lines a runner emits when something breaks.
pub const ERROR_MARKERS: &[&str] = &[
    "error",
    "failed",
    "failure",
    "panicked",
    "assertion",
    "exit code",
    "not ok",
    "fatal",
];

/// Reduce a failed-step log to one normalized line that survives a rerun.
///
/// Reruns of the same regression differ in timestamps, durations, run numbers,
/// ANSI styling, and paths under a run-specific temp directory. Normalizing
/// those away is what lets an hourly sweep recognize the same root cause
/// instead of filing it again every hour.
///
/// Preference order, so a generic wrapper cannot fragment one evidenced
/// failure or collapse distinct ones:
/// 1. A coded compiler diagnostic (`error[E0062]: …`).
/// 2. A concrete test/panic identity (`thread '…' panicked`, `test … FAILED`,
///    nextest `FAIL […]`, a name listed after libtest `failures:`).
/// 3. A specific `##[error]` annotation.
/// 4. Any remaining marker diagnostic (compiler `error:`, `assertion failed`).
/// 5. The nearest unannotated content line with letters or digits before a generic trailer.
/// 6. The failing step name, labelled as a fallback — used when the excerpt
///    only has wrappers, bookkeeping, or assertion payload.
///
/// Generic trailers include GitHub's process-completed / `The process '…'
/// failed with exit code` / action-failed annotations, cargo's
/// `test failed, to rerun pass` wrappers, and nextest cancellation/summary
/// lines. Assertion `left:`/`right:` dumps are not signatures even when they
/// contain marker words. Raw excerpt bytes stay in the filed description;
/// ANSI is stripped only for classification and the normalized signature.
pub fn error_signature(log_excerpt: &str, step: &str) -> ErrorSignature {
    let lines = classify_log_lines(log_excerpt);
    if let Some(index) = diagnostic_anchor(&lines) {
        return ErrorSignature {
            text: normalize_signature(&signature_payload(lines[index].1)),
            step_fallback: false,
        };
    }
    ErrorSignature {
        text: normalize_signature(&step.to_ascii_lowercase()),
        step_fallback: true,
    }
}

/// Signature and display must agree on the strongest diagnostic, independent
/// of where setup output or a process-exit wrapper appears in the command.
pub fn diagnostic_anchor(lines: &[(LineKind, &str)]) -> Option<usize> {
    for wanted in [
        LineKind::CompilerDiagnostic,
        LineKind::ConcreteDiagnostic,
        LineKind::ErrorAnnotated,
        LineKind::Marker,
    ] {
        if let Some(index) = lines.iter().position(|(kind, _)| *kind == wanted) {
            return Some(index);
        }
    }
    for (index, (kind, _)) in lines.iter().enumerate() {
        if *kind == LineKind::GenericTrailer
            && let Some(previous) = lines[..index]
                .iter()
                .rposition(|(kind, line)| *kind == LineKind::Content && is_diagnostic_content(line))
        {
            return Some(previous);
        }
    }
    None
}

fn is_compiler_diagnostic(payload: &str) -> bool {
    let payload = payload.strip_prefix("##[error]").unwrap_or(payload).trim();
    let Some(rest) = payload.strip_prefix("error[e") else {
        return false;
    };
    let Some((code, message)) = rest.split_once("]:") else {
        return false;
    };
    code.len() == 4 && code.bytes().all(|byte| byte.is_ascii_digit()) && !message.trim().is_empty()
}

fn is_cargo_status(payload: &str) -> bool {
    [
        "compiling ",
        "checking ",
        "downloading ",
        "downloaded ",
        "fresh ",
        "error: could not compile ",
        "warning: build failed",
        "for more information about this error",
    ]
    .iter()
    .any(|prefix| payload.starts_with(prefix))
}

/// A failed step's normalized root-cause line.
pub struct ErrorSignature {
    pub text: String,
    /// True when no diagnostic line survived and `text` is the step name.
    pub step_fallback: bool,
}

/// What one runner log line is, for signature and excerpt selection.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    RunCommand,
    ParamDump,
    EndGroup,
    CompilerDiagnostic,
    CargoStatus,
    ConcreteDiagnostic,
    ErrorAnnotated,
    GenericTrailer,
    Marker,
    Bookkeeping,
    Content,
}

/// Classify each line of a failed-step log.
pub fn classify_log_lines(log: &str) -> Vec<(LineKind, &str)> {
    let mut in_param_block = false;
    let mut after_failures_header = false;
    let mut out = Vec::new();
    for line in log.lines() {
        let payload = signature_payload(line);
        let lowered = payload.trim();
        let indented = payload.starts_with(' ') || payload.starts_with('\t');
        let kind = if is_run_command_payload(lowered) {
            in_param_block = false;
            LineKind::RunCommand
        } else if lowered.contains("##[endgroup]") {
            in_param_block = false;
            LineKind::EndGroup
        } else if lowered == "env:" || lowered == "with:" {
            in_param_block = true;
            LineKind::ParamDump
        } else if in_param_block && (indented || lowered.is_empty()) {
            LineKind::ParamDump
        } else {
            in_param_block = false;
            if is_generic_trailer(lowered) {
                LineKind::GenericTrailer
            } else if is_compiler_diagnostic(lowered) {
                LineKind::CompilerDiagnostic
            } else if is_cargo_status(lowered) {
                LineKind::CargoStatus
            } else if lowered.contains("##[error]") {
                LineKind::ErrorAnnotated
            } else if is_runner_bookkeeping(lowered) || lowered.contains("##[group]") {
                LineKind::Bookkeeping
            } else if is_concrete_diagnostic(&payload, lowered, after_failures_header) {
                LineKind::ConcreteDiagnostic
            } else if is_error_marker_line(lowered) && !is_assertion_payload(lowered) {
                LineKind::Marker
            } else {
                LineKind::Content
            }
        };
        if lowered == "failures:" || lowered == "errors:" {
            after_failures_header = true;
        } else if is_libtest_stdout_header(lowered) || lowered.starts_with("test result:") {
            after_failures_header = false;
        }
        out.push((kind, line));
    }
    out
}

/// A runner, cargo or nextest wrapper line that names no specific cause.
pub fn is_generic_trailer(lowered: &str) -> bool {
    is_generic_runner_completion(lowered)
        || is_generic_process_failed(lowered)
        || is_generic_action_failed(lowered)
        || is_cargo_test_wrapper(lowered)
        || is_nextest_cancellation(lowered)
        || is_nextest_summary(lowered)
        || lowered.contains("tests were not run due to test failure")
}

fn is_generic_runner_completion(lowered: &str) -> bool {
    let Some(message) = lowered.trim().strip_prefix("##[error]") else {
        return false;
    };
    let Some(exit_code) = message
        .trim()
        .strip_prefix("process completed with exit code ")
    else {
        return false;
    };
    let exit_code = exit_code.trim_end_matches('.');
    !exit_code.is_empty() && exit_code.chars().all(|ch| ch.is_ascii_digit())
}

fn is_generic_process_failed(lowered: &str) -> bool {
    let Some(message) = lowered.trim().strip_prefix("##[error]") else {
        return false;
    };
    let message = message.trim().trim_end_matches('.');
    let Some(rest) = message.strip_prefix("the process ") else {
        return false;
    };
    rest.contains(" failed with exit code ")
}

fn is_generic_action_failed(lowered: &str) -> bool {
    let Some(message) = lowered.trim().strip_prefix("##[error]") else {
        return false;
    };
    let message = message
        .trim()
        .trim_start_matches(|ch: char| !ch.is_ascii_alphabetic());
    message == "action failed"
}

fn is_cargo_test_wrapper(lowered: &str) -> bool {
    let message = lowered
        .trim()
        .strip_prefix("error:")
        .map(str::trim)
        .unwrap_or_else(|| lowered.trim());
    message.starts_with("test failed, to rerun pass")
        || message == "test run failed"
        || message.starts_with("process didn't exit successfully:")
}

fn is_nextest_cancellation(lowered: &str) -> bool {
    lowered
        .trim()
        .trim_end_matches(':')
        .trim()
        .starts_with("cancelling due to test failure")
}

fn is_nextest_summary(lowered: &str) -> bool {
    let trimmed = lowered.trim();
    trimmed.starts_with("summary [") && trimmed.contains("tests run:")
}

fn is_concrete_diagnostic(payload: &str, lowered: &str, after_failures_header: bool) -> bool {
    is_panic_line(lowered)
        || is_failed_test_result(lowered)
        || is_nextest_fail_line(lowered)
        || is_libtest_listed_failure_name(payload, after_failures_header)
}

fn is_panic_line(lowered: &str) -> bool {
    lowered.contains("panicked at")
        && (lowered.contains("thread '") || lowered.contains("thread \""))
}

fn is_failed_test_result(lowered: &str) -> bool {
    let Some(rest) = lowered.strip_prefix("test ") else {
        return false;
    };
    let Some((_, status)) = rest.rsplit_once(" ... ") else {
        return false;
    };
    let status = status.trim();
    status == "failed" || status.starts_with("failed ")
}

fn is_nextest_fail_line(lowered: &str) -> bool {
    let Some(rest) = lowered.trim().strip_prefix("fail") else {
        return false;
    };
    rest.trim_start().starts_with('[')
}

pub fn is_libtest_stdout_header(lowered: &str) -> bool {
    let trimmed = lowered.trim();
    trimmed.starts_with("---- ")
        && (trimmed.ends_with(" stdout ----") || trimmed.ends_with(" stderr ----"))
}

fn is_libtest_listed_failure_name(payload: &str, after_failures_header: bool) -> bool {
    if !after_failures_header {
        return false;
    }
    let indented = payload.starts_with(' ') || payload.starts_with('\t');
    if !indented {
        return false;
    }
    let trimmed = payload.trim();
    !trimmed.is_empty()
        && !trimmed.contains(' ')
        && !trimmed.starts_with("----")
        && !trimmed.starts_with("thread")
        && !trimmed.starts_with("error")
        && !trimmed.starts_with("note:")
        && !trimmed.starts_with("assertion")
}

fn is_assertion_payload(lowered: &str) -> bool {
    let trimmed = lowered.trim_start();
    trimmed.starts_with("left:")
        || trimmed.starts_with("right:")
        || trimmed.starts_with("left =")
        || trimmed.starts_with("right =")
}

fn is_diagnostic_content(line: &str) -> bool {
    let payload = signature_payload(line);
    let trimmed = payload.trim();
    trimmed.chars().any(char::is_alphanumeric) && !trimmed.starts_with("##[")
}

/// The `##[group]Run …` line that opens a step's command.
pub fn is_run_command_payload(payload: &str) -> bool {
    let lowered = payload.trim().to_ascii_lowercase();
    lowered.starts_with("##[group]run ") || lowered == "##[group]run"
}

fn is_runner_bookkeeping(lowered: &str) -> bool {
    lowered.starts_with("head is now at") || lowered.starts_with("syncing repository")
}

/// Unanchored marker hit that is an actual diagnostic, not a passing test or
/// cargo/libtest section header. Those headers are identical across distinct
/// panics, and success lines often contain `error`/`failure` in the test name.
pub fn is_error_marker_line(lowered: &str) -> bool {
    if is_libtest_non_diagnostic(lowered) {
        return false;
    }
    ERROR_MARKERS.iter().any(|marker| lowered.contains(marker))
}

fn is_libtest_non_diagnostic(lowered: &str) -> bool {
    let trimmed = lowered.trim();
    matches!(trimmed, "failures:" | "errors:" | "successes:")
        || trimmed.starts_with("test result:")
        || is_successful_test_result(trimmed)
}

/// `test <name> ... ok` / `ignored`, with an optional timing suffix.
fn is_successful_test_result(lowered: &str) -> bool {
    let Some(rest) = lowered.strip_prefix("test ") else {
        return false;
    };
    let Some((_, status)) = rest.rsplit_once(" ... ") else {
        return false;
    };
    let status = status.trim();
    status == "ok"
        || status.starts_with("ok ")
        || status == "ignored"
        || status.starts_with("ignored ")
}

/// Cap `text` at `max_bytes` while keeping `anchor_line`, not the head.
/// Strip the `job<TAB>step<TAB>timestamp ` columns a runner log carries.
pub fn log_payload(line: &str) -> &str {
    let mut columns = line.splitn(3, '\t');
    let rest = match (columns.next(), columns.next(), columns.next()) {
        (Some(_job), Some(_step), Some(rest)) => rest,
        _ => line,
    };
    match rest.split_once(' ') {
        Some((first, tail)) if first.contains('T') && first.ends_with('Z') => tail,
        _ => rest,
    }
}

/// Payload used for classification and the normalized signature: runner
/// columns removed, ANSI styling stripped, lowercased. The filed excerpt keeps
/// the raw line so evidence is not discarded.
pub fn signature_payload(line: &str) -> String {
    strip_ansi_sequences(log_payload(line)).to_ascii_lowercase()
}

/// Collapse the parts of a log line that vary between identical failures:
/// bare numbers, long hex blobs, and measurements whose unit is the only
/// stable part (nextest's `FAIL [ 1.399s]` is a different duration on every
/// rerun of the same failing test).
pub fn normalize_signature(lowered: &str) -> String {
    let mut out = String::with_capacity(lowered.len());
    let mut chars = lowered.chars().peekable();
    let mut last_was_space = false;
    while let Some(ch) = chars.next() {
        if ch.is_ascii_alphanumeric() {
            let mut token = String::from(ch);
            while chars.peek().is_some_and(char::is_ascii_alphanumeric) {
                token.push(chars.next().unwrap_or_default());
            }
            // The token is ASCII alphanumeric, so a digit count indexes it directly.
            let digits = token.chars().take_while(char::is_ascii_digit).count();
            if digits == token.len() {
                out.push_str("<n>");
            } else if token.len() >= 7 && token.chars().all(|c| c.is_ascii_hexdigit()) {
                out.push_str("<hex>");
            } else if digits > 0 && token[digits..].chars().all(|c| c.is_ascii_alphabetic()) {
                // A measurement such as `399s` or `250ms`: keep the unit, drop the count.
                out.push_str("<n>");
                out.push_str(&token[digits..]);
            } else {
                out.push_str(&token);
            }
            last_was_space = false;
            continue;
        }
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
            continue;
        }
        out.push(ch);
        last_was_space = false;
    }
    let out = out.trim();
    if out.chars().count() <= MAX_SIGNATURE_CHARS {
        return out.to_string();
    }
    out.chars().take(MAX_SIGNATURE_CHARS).collect::<String>() + "…"
}

/// Longest normalized signature kept; longer ones end in `…`.
const MAX_SIGNATURE_CHARS: usize = 200;
