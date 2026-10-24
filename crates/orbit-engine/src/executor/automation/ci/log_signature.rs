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
/// 2. A concrete test/panic identity: first the harness verdict naming the
///    failing test (`test … FAILED`, nextest `FAIL […]`, a name listed after
///    libtest `failures:`), then that test's captured `thread '…' panicked`
///    output, which is detail of the same failure rather than its identity.
/// 3. A specific `##[error]` annotation.
/// 4. A labelled diagnostic (`error[vulnerability]: …`).
/// 5. Any remaining marker diagnostic (compiler `error:`, `assertion failed`).
/// 6. The nearest unannotated content line with letters or digits before a generic trailer.
/// 7. The failing step name, labelled as a fallback — used when the excerpt
///    only has wrappers, bookkeeping, or assertion payload.
///
/// Within one rank (and within the two concrete tiers) diagnostics prefer
/// the last one before the process-exit trailer.
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
    if lines
        .last()
        .is_some_and(|(kind, _)| matches!(kind, LineKind::RunCommand | LineKind::CommandBody))
    {
        return None;
    }
    // Cleanup output after the failing process exited is not its cause.
    let end = lines
        .iter()
        .rposition(|(kind, line)| {
            *kind == LineKind::GenericTrailer && {
                let payload = signature_payload(line);
                is_generic_runner_completion(&payload) || is_generic_process_failed(&payload)
            }
        })
        .map_or(lines.len(), |index| index + 1);
    let lines = &lines[..end];
    if let Some(index) = lines
        .iter()
        .rposition(|(kind, _)| *kind == LineKind::CompilerDiagnostic)
    {
        return Some(index);
    }
    // A harness verdict names the failing test; a `thread '…' panicked` line
    // below it is that same test's captured output. Preferring the verdict
    // keeps one stable identity when both are emitted (for example nextest's
    // `FAIL […]` line followed by the test's panic detail).
    if let Some(index) = lines
        .iter()
        .rposition(|(kind, line)| *kind == LineKind::ConcreteDiagnostic && !is_captured_panic(line))
    {
        return Some(index);
    }
    if let Some(index) = lines
        .iter()
        .rposition(|(kind, _)| *kind == LineKind::ConcreteDiagnostic)
    {
        return Some(index);
    }
    for wanted in [
        LineKind::ErrorAnnotated,
        LineKind::LabelledDiagnostic,
        LineKind::Marker,
    ] {
        if let Some(index) = lines.iter().rposition(|(kind, _)| *kind == wanted) {
            return Some(index);
        }
    }
    for (index, (kind, _)) in lines.iter().enumerate().rev() {
        if *kind == LineKind::GenericTrailer
            && let Some(previous) = lines[..index]
                .iter()
                .rposition(|(kind, line)| *kind == LineKind::Content && is_diagnostic_content(line))
            && !lines[previous + 1..index].iter().any(|(kind, _)| {
                matches!(
                    kind,
                    LineKind::RunCommand | LineKind::CommandBody | LineKind::EndGroup
                )
            })
        {
            return Some(previous);
        }
    }
    None
}

/// Bounded raw evidence around the same emitted diagnostic used for identity.
/// With no diagnostic, retain bounded context without asserting a cause.
pub fn diagnostic_excerpt(log: &str, max_bytes: usize) -> String {
    let lines = classify_log_lines(log);
    let anchor = diagnostic_anchor(&lines);
    let offset = anchor
        .map(|index| {
            log.split_inclusive('\n')
                .take(index)
                .map(str::len)
                .sum::<usize>()
        })
        .unwrap_or(0);
    let anchor_len = anchor.map(|index| lines[index].1.len()).unwrap_or(0);
    let mut start = offset.saturating_sub(max_bytes.saturating_sub(anchor_len) / 3);
    while !log.is_char_boundary(start) {
        start += 1;
    }
    let mut end = start.saturating_add(max_bytes).min(log.len());
    while !log.is_char_boundary(end) {
        end -= 1;
    }
    log[start..end].to_string()
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

fn is_labelled_diagnostic(payload: &str) -> bool {
    let Some((label, message)) = payload
        .strip_prefix("error[")
        .and_then(|rest| rest.split_once("]:"))
    else {
        return false;
    };
    label.bytes().any(|byte| byte.is_ascii_alphabetic())
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':'))
        && !message.trim().is_empty()
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
    /// Printed source inside the runner command group, never emitted evidence.
    CommandBody,
    ParamDump,
    EndGroup,
    CompilerDiagnostic,
    CargoStatus,
    ConcreteDiagnostic,
    ErrorAnnotated,
    /// A line-initial named diagnostic, for example cargo-deny's vulnerability.
    LabelledDiagnostic,
    GenericTrailer,
    Marker,
    Bookkeeping,
    Content,
}

/// Classify each line of a failed-step log. A runner's command display lasts
/// from `Run` through its unindented closing group; its body is source context,
/// including lines that resemble compiler, test, or annotation output.
pub fn classify_log_lines(log: &str) -> Vec<(LineKind, &str)> {
    let mut in_param_block = false;
    // An orphan closing group can be the tail of a printed command whose
    // opener was omitted. Keep that prefix as context, never as a cause.
    let mut in_command = log
        .lines()
        .find_map(|line| {
            let payload = signature_payload(line);
            if payload.starts_with("##[group]") {
                Some(false)
            } else if payload.trim_end() == "##[endgroup]" {
                Some(true)
            } else {
                None
            }
        })
        .unwrap_or(false);
    let mut after_failures_header = false;
    let mut out = Vec::new();
    for line in log.lines() {
        let payload = signature_payload(line);
        let lowered = payload.trim();
        let indented = payload.starts_with(' ') || payload.starts_with('\t');
        let kind = if in_command && payload.trim_end() != "##[endgroup]" {
            LineKind::CommandBody
        } else if payload.trim_end() == "##[endgroup]" {
            in_command = false;
            in_param_block = false;
            LineKind::EndGroup
        } else if is_run_command_payload(lowered) {
            in_command = true;
            in_param_block = false;
            after_failures_header = false;
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
            } else if is_runner_bookkeeping(lowered)
                || is_successful_ci_test_route_summary(lowered)
                || lowered.contains("##[group]")
            {
                LineKind::Bookkeeping
            } else if is_concrete_diagnostic(&payload, lowered, after_failures_header) {
                LineKind::ConcreteDiagnostic
            } else if is_labelled_diagnostic(lowered) {
                LineKind::LabelledDiagnostic
            } else if is_error_marker_line(lowered) && !is_assertion_payload(lowered) {
                LineKind::Marker
            } else {
                LineKind::Content
            }
        };
        if matches!(kind, LineKind::RunCommand | LineKind::CommandBody) {
            after_failures_header = false;
        } else if lowered == "failures:" || lowered == "errors:" {
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

/// Captured `thread '…' panicked` output from the failing test itself, as
/// opposed to the harness verdict lines that name that test. Partitioning
/// concrete diagnostics this way is exact: a verdict line never contains a
/// panic attribution, and a panic line always contains spaces, so it never
/// matches the verdict shapes (`FAIL [`, `test … ... FAILED`, or a bare
/// listed failure name).
fn is_captured_panic(line: &str) -> bool {
    is_panic_line(&signature_payload(line))
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

/// `check-ci-macos` prints this successful routing summary after a filtered
/// test selector matches. A test name can contain any error marker word, but
/// the exact summary shape is status output rather than a diagnostic.
fn is_successful_ci_test_route_summary(lowered: &str) -> bool {
    let Some(summary) = lowered.strip_prefix("check-ci-macos: ") else {
        return false;
    };
    let Some((_, matched)) = summary.rsplit_once(" matched ") else {
        return false;
    };
    let Some(count) = matched.strip_suffix(" tests") else {
        return false;
    };
    !count.is_empty() && count.bytes().all(|byte| byte.is_ascii_digit())
}

/// Unanchored marker hit that is an actual diagnostic, not a passing test or
/// cargo/libtest section header. Those headers are identical across distinct
/// panics, and success lines often contain `error`/`failure` in the test name.
pub fn is_error_marker_line(lowered: &str) -> bool {
    if is_libtest_non_diagnostic(lowered) {
        return false;
    }
    ERROR_MARKERS.iter().any(|marker| {
        lowered.match_indices(marker).any(|(start, _)| {
            !lowered[..start]
                .chars()
                .next_back()
                .is_some_and(is_identifier_or_path_char)
                && !lowered[start + marker.len()..]
                    .chars()
                    .next()
                    .is_some_and(is_identifier_or_path_char)
        })
    })
}

fn is_identifier_or_path_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '\\')
}

fn is_libtest_non_diagnostic(lowered: &str) -> bool {
    let trimmed = lowered.trim();
    matches!(trimmed, "failures:" | "errors:" | "successes:")
        || trimmed.starts_with("test result:")
        || is_successful_test_result(trimmed)
        || is_tap_non_diagnostic(trimmed)
}

/// TAP captions and successful result lines are status output even when their
/// test names contain words such as `failed` or `error`.
fn is_tap_non_diagnostic(lowered: &str) -> bool {
    if lowered.starts_with("# subtest:") {
        return true;
    }

    let Some(rest) = lowered.strip_prefix("ok ") else {
        return false;
    };
    let Some((number, result)) = rest.split_once(' ') else {
        return false;
    };
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }

    if result.starts_with("- ") {
        return true;
    }
    let Some(directive) = result.strip_prefix("# ") else {
        return false;
    };
    matches!(directive.split_whitespace().next(), Some("skip" | "todo"))
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

/// Strip a raw timestamp or the `job<TAB>step<TAB>timestamp ` runner columns,
/// preserving payload tabs and indentation used to distinguish printed source.
pub fn log_payload(line: &str) -> &str {
    // Raw job API logs have no job/step columns. Their payload can contain
    // arbitrary tabs, so recognize the leading timestamp before splitting.
    if let Some(payload) = timestamp_payload(line) {
        return payload;
    }
    let mut columns = line.splitn(3, '\t');
    let rest = match (columns.next(), columns.next(), columns.next()) {
        (Some(_job), Some(_step), Some(rest)) => rest,
        _ => line,
    };
    timestamp_payload(rest).unwrap_or(rest)
}

fn timestamp_payload(line: &str) -> Option<&str> {
    line.split_once(' ')
        .filter(|(prefix, _)| {
            prefix.ends_with('Z') && prefix.contains('T') && !prefix.contains('\t')
        })
        .map(|(_, payload)| payload)
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
