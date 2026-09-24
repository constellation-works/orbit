//! Failed-step log analysis for CI failure filing: line classification, the
//! bounded description excerpt, root-cause signatures, and the distinctive
//! anchors a repair brief can quote.

use std::collections::BTreeSet;

use orbit_tools::github_cli::strip_ansi_sequences;
use serde_json::Value;

use super::{truncate_bytes, truncate_chars, value_string};

/// Distinctive diagnostic lines a manual repair brief can quote without
/// generated `workflow` / `failing job` / `failing step` labels.
pub(super) fn specific_error_anchors(log: &str, signature: &str) -> Vec<String> {
    let mut anchors: Vec<String> = Vec::new();
    let mut push = |value: &str| {
        let Some(normalized) = specific_error_text(value) else {
            return;
        };
        if !anchors
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&normalized))
        {
            anchors.push(normalized);
        }
    };
    push(signature);
    for (kind, line) in classify_log_lines(log) {
        if !matches!(
            kind,
            LineKind::CompilerDiagnostic
                | LineKind::ConcreteDiagnostic
                | LineKind::ErrorAnnotated
                | LineKind::Marker
                | LineKind::Content
        ) {
            continue;
        }
        push(&strip_ansi_sequences(log_payload(line)));
    }
    anchors
}

fn specific_error_text(value: &str) -> Option<String> {
    let trimmed = value
        .trim()
        .trim_start_matches(|character: char| {
            character == '-' || character == '*' || character.is_whitespace()
        })
        .trim();
    if trimmed.chars().count() < 16 || trimmed.chars().count() > 180 {
        return None;
    }
    let lowered = trimmed.to_ascii_lowercase();
    if is_generic_trailer(&lowered)
        || is_run_command_payload(&lowered)
        || lowered.starts_with("[command]")
        || (lowered.contains("added ") && lowered.contains("packages"))
        || lowered.contains("looking for funding")
        || lowered.contains("found 0 vulnerabilities")
        || lowered.contains("wrangler installed")
        || lowered.contains("logs were written")
    {
        return None;
    }
    let diagnostic = ERROR_MARKERS.iter().any(|marker| lowered.contains(marker))
        || lowered.contains("missing")
        || lowered.contains("not found")
        || lowered.contains("cannot")
        || lowered.contains("invalid")
        || lowered.contains("expected")
        || lowered.contains("required");
    diagnostic.then(|| trimmed.to_string())
}

pub(super) fn specific_command_from_log(log: &str) -> Option<String> {
    let mut best = None;
    for (kind, line) in classify_log_lines(log) {
        let payload = log_payload(line);
        let raw = if kind == LineKind::RunCommand {
            run_command_body(payload)
        } else {
            bracket_command_body(payload)
        };
        let Some(raw) = raw else {
            continue;
        };
        if let Some(stable) = stabilize_command(raw) {
            best = Some(stable);
        }
    }
    best
}

fn run_command_body(payload: &str) -> Option<&str> {
    let trimmed = payload.trim();
    let lower = trimmed.to_ascii_lowercase();
    const PREFIX: &str = "##[group]run ";
    lower
        .starts_with(PREFIX)
        .then(|| trimmed.get(PREFIX.len()..).unwrap_or_default().trim())
        .filter(|body| !body.is_empty())
}

fn bracket_command_body(payload: &str) -> Option<&str> {
    let trimmed = payload.trim();
    let lower = trimmed.to_ascii_lowercase();
    const PREFIX: &str = "[command]";
    lower
        .starts_with(PREFIX)
        .then(|| trimmed.get(PREFIX.len()..).unwrap_or_default().trim())
        .filter(|body| !body.is_empty())
}

fn stabilize_command(raw: &str) -> Option<String> {
    let mut tokens = raw.split_whitespace().collect::<Vec<_>>();
    if tokens
        .first()
        .is_some_and(|token| token.eq_ignore_ascii_case("[command]"))
    {
        tokens.remove(0);
    }
    while tokens
        .first()
        .is_some_and(|token| is_javascript_runtime_token(token))
    {
        tokens.remove(0);
        if tokens.first().is_some_and(|token| {
            matches!(*token, "--no-install" | "--yes" | "-y" | "--prefer-offline")
        }) {
            tokens.remove(0);
        }
    }
    if tokens.is_empty() || is_install_invocation(&tokens) {
        return None;
    }
    let mut kept = Vec::new();
    let mut skip_value = false;
    for token in tokens {
        if skip_value {
            skip_value = false;
            continue;
        }
        if is_volatile_command_flag(token) {
            if !token.contains('=') {
                skip_value = true;
            }
            continue;
        }
        kept.push(token);
    }
    if is_generic_command(&kept) {
        return None;
    }
    Some(kept.join(" "))
}

fn is_javascript_runtime_token(token: &str) -> bool {
    let base = token
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(token)
        .to_ascii_lowercase();
    matches!(base.as_str(), "npx" | "npm" | "node" | "yarn" | "pnpm")
}

fn is_install_invocation(tokens: &[&str]) -> bool {
    matches!(
        tokens
            .first()
            .map(|token| token.to_ascii_lowercase())
            .as_deref(),
        Some("i" | "install" | "add" | "ci")
    )
}

fn is_volatile_command_flag(token: &str) -> bool {
    let name = token
        .trim_start_matches('-')
        .split_once('=')
        .map(|(name, _)| name)
        .unwrap_or_else(|| token.trim_start_matches('-'));
    matches!(
        name,
        "commit-hash" | "commit-message" | "commit" | "sha" | "hash"
    )
}

fn is_generic_command(tokens: &[&str]) -> bool {
    if tokens.is_empty() {
        return true;
    }
    let joined = tokens.join(" ").to_ascii_lowercase();
    if matches!(
        joined.as_str(),
        "cargo test"
            | "cargo build"
            | "cargo check"
            | "cargo clippy"
            | "cargo nextest run"
            | "cargo llvm-cov"
            | "npm test"
            | "npm run build"
            | "yarn test"
            | "pnpm test"
            | "npx"
            | "node"
            | "wrangler --version"
            | "wrangler version"
    ) {
        return true;
    }
    let distinctive = tokens.iter().any(|token| {
        token.contains('/')
            || token.contains('\\')
            || (token.starts_with("--") && token.contains('='))
            || token.contains('@')
    });
    tokens.len() < 3 && !distinctive
}

/// Lines a runner emits when something breaks.
const ERROR_MARKERS: &[&str] = &[
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
/// 5. The nearest unannotated content line before a generic trailer.
/// 6. The failing step name, labelled as a fallback — used when the excerpt
///    only has wrappers, bookkeeping, or assertion payload.
///
/// Generic trailers include GitHub's process-completed / `The process '…'
/// failed with exit code` / action-failed annotations, cargo's
/// `test failed, to rerun pass` wrappers, and nextest cancellation/summary
/// lines. Assertion `left:`/`right:` dumps are not signatures even when they
/// contain marker words. Raw excerpt bytes stay in the filed description;
/// ANSI is stripped only for classification and the normalized signature.
pub(super) fn error_signature(log_excerpt: &str, step: &str) -> ErrorSignature {
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
fn diagnostic_anchor(lines: &[(LineKind, &str)]) -> Option<usize> {
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

/// Preserve the shipped signature algorithm only for looking up existing tags.
pub(super) fn legacy_signature(lines: &[(LineKind, &str)], step: &str) -> String {
    let legacy: Vec<_> = lines
        .iter()
        .map(|(kind, line)| {
            let kind = match kind {
                LineKind::CompilerDiagnostic | LineKind::CargoStatus => {
                    if signature_payload(line).contains("##[error]") {
                        LineKind::ErrorAnnotated
                    } else if is_error_marker_line(signature_payload(line).trim()) {
                        LineKind::Marker
                    } else {
                        LineKind::Content
                    }
                }
                other => *other,
            };
            (kind, *line)
        })
        .collect();
    diagnostic_anchor(&legacy)
        .map(|index| normalize_signature(&signature_payload(legacy[index].1)))
        .unwrap_or_else(|| normalize_signature(&step.to_ascii_lowercase()))
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

/// A conservative proof for cross-job merging. Preserve diagnostic operands
/// and line/column numbers: display normalization deliberately erases numbers
/// and truncates text, so it is not strong enough for a compiler cause key.
pub(super) fn compiler_cause(log: &str) -> Option<String> {
    let lines = classify_log_lines(log);
    let mut causes = BTreeSet::new();
    for (index, (kind, line)) in lines.iter().enumerate() {
        if *kind != LineKind::CompilerDiagnostic {
            continue;
        }
        let diagnostic = strip_ansi_sequences(log_payload(line));
        let diagnostic = diagnostic
            .trim()
            .strip_prefix("##[error]")
            .unwrap_or(diagnostic.trim())
            .trim();
        let location = lines
            .get(index + 1)
            .map(|(_, line)| strip_ansi_sequences(log_payload(line)))?;
        let location = location.trim().strip_prefix("-->")?.trim();
        let (path_line, column) = location.rsplit_once(':')?;
        let (path, line_number) = path_line.rsplit_once(':')?;
        if path.starts_with('/')
            || path.contains("..")
            || !path.ends_with(".rs")
            || line_number.parse::<u64>().is_err()
            || column.parse::<u64>().is_err()
        {
            return None;
        }
        causes.insert(format!("{diagnostic} @ {location}"));
    }
    (!causes.is_empty()).then(|| causes.into_iter().collect::<Vec<_>>().join("; "))
}

pub(super) struct ErrorSignature {
    pub(super) text: String,
    pub(super) step_fallback: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LineKind {
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

impl LineKind {
    fn skip_from_excerpt(self) -> bool {
        matches!(
            self,
            Self::ParamDump | Self::EndGroup | Self::Bookkeeping | Self::CargoStatus
        )
    }
}

pub(super) struct FailedStepExcerpt {
    pub(super) body: String,
    pub(super) has_anchor: bool,
}

/// Command line plus the failure region, never a head-biased env dump.
///
/// The `##[group]Run …` line is useful reproduction context. The selected diagnostic and its following
/// source location take precedence over oversized wrapper arguments. The
/// `env:` / `with:` dump that follows the command is never the evidence.
/// The remaining block is a bounded window around the diagnostic, capped at
/// `max_bytes` on that region rather than the log head.
pub(super) fn render_failed_step_excerpt(log: &str, max_bytes: usize) -> FailedStepExcerpt {
    let lines = classify_log_lines(log);
    let command = lines
        .iter()
        .find(|(kind, _)| *kind == LineKind::RunCommand)
        .map(|(_, line)| *line);

    let anchor = diagnostic_anchor(&lines).or_else(|| {
        lines
            .iter()
            .position(|(kind, _)| *kind == LineKind::GenericTrailer)
    });

    let Some(anchor_idx) = anchor else {
        return FailedStepExcerpt {
            body: command.unwrap_or("").to_string(),
            has_anchor: false,
        };
    };

    const LINES_BEFORE: usize = 24;
    const LINES_AFTER: usize = 12;
    let kept: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, (kind, _))| !kind.skip_from_excerpt())
        .map(|(idx, _)| idx)
        .collect();
    let anchor_in_kept = kept.iter().position(|idx| *idx == anchor_idx).unwrap_or(0);
    let start = anchor_in_kept.saturating_sub(LINES_BEFORE);
    let end = (anchor_in_kept + 1 + LINES_AFTER).min(kept.len());
    let mut window: Vec<&str> = kept[start..end].iter().map(|idx| lines[*idx].1).collect();
    if let Some(command) = command
        && !window.contains(&command)
    {
        window.insert(0, command);
    }
    let joined = window.join("\n");
    FailedStepExcerpt {
        body: cap_bytes_around_line(&joined, lines[anchor_idx].1, max_bytes),
        has_anchor: true,
    }
}

pub(super) fn classify_log_lines(log: &str) -> Vec<(LineKind, &str)> {
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

fn is_generic_trailer(lowered: &str) -> bool {
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

pub(super) fn is_libtest_stdout_header(lowered: &str) -> bool {
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
    !trimmed.is_empty() && !trimmed.starts_with("##[")
}

fn is_run_command_payload(payload: &str) -> bool {
    let lowered = payload.trim().to_ascii_lowercase();
    lowered.starts_with("##[group]run ") || lowered == "##[group]run"
}

fn is_runner_bookkeeping(lowered: &str) -> bool {
    lowered.starts_with("head is now at") || lowered.starts_with("syncing repository")
}

/// Unanchored marker hit that is an actual diagnostic, not a passing test or
/// cargo/libtest section header. Those headers are identical across distinct
/// panics, and success lines often contain `error`/`failure` in the test name.
fn is_error_marker_line(lowered: &str) -> bool {
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
fn cap_bytes_around_line(text: &str, anchor_line: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let Some(anchor_start) = text.find(anchor_line) else {
        return truncate_bytes(text, max_bytes);
    };
    let anchor_len = anchor_line.len();
    if anchor_len >= max_bytes {
        return truncate_bytes(anchor_line, max_bytes);
    }
    let extra = max_bytes - anchor_len;
    let want_before = extra / 3;
    let mut start = anchor_start.saturating_sub(want_before);
    while start < anchor_start && !text.is_char_boundary(start) {
        start += 1;
    }
    if start > 0
        && let Some(newline) = text[start..anchor_start].find('\n')
    {
        start += newline + 1;
    }
    let mut end = start.saturating_add(max_bytes).min(text.len());
    if end < anchor_start + anchor_len {
        end = (anchor_start + anchor_len).min(text.len());
        start = end.saturating_sub(max_bytes);
        while start > 0 && !text.is_char_boundary(start) {
            start -= 1;
        }
    }
    while end > anchor_start + anchor_len && !text.is_char_boundary(end) {
        end -= 1;
    }
    if end < text.len()
        && let Some(newline) = text[anchor_start + anchor_len..end].rfind('\n')
    {
        end = anchor_start + anchor_len + newline;
    }
    let mut out = String::new();
    if start > 0 {
        out.push_str("[...]\n");
    }
    out.push_str(&text[start..end]);
    if end < text.len() {
        out.push_str(&format!(
            "\n[... truncated at {max_bytes} B for the task description; the full excerpt is in \
             the sweep run's step output ...]"
        ));
    }
    out
}

/// `query_errors` entries for this cluster's failed-step log fetch, if any.
pub(super) fn relevant_log_query_errors<'a>(evidence: &'a Value, runs: &[Value]) -> Vec<&'a Value> {
    let run_ids: BTreeSet<String> = runs
        .iter()
        .map(|run| value_string(run, "run_id"))
        .filter(|id| !id.is_empty())
        .collect();
    evidence
        .get("query_errors")
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter(|error| {
                    let query = value_string(error, "query");
                    let run_id = value_string(error, "run_id");
                    matches!(query.as_str(), "run_logs" | "run_logs_all")
                        && run_ids.contains(&run_id)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Strip the `job<TAB>step<TAB>timestamp ` columns a runner log carries.
fn log_payload(line: &str) -> &str {
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
pub(super) fn signature_payload(line: &str) -> String {
    strip_ansi_sequences(log_payload(line)).to_ascii_lowercase()
}

/// Collapse the parts of a log line that vary between identical failures:
/// bare numbers, long hex blobs, and measurements whose unit is the only
/// stable part (nextest's `FAIL [ 1.399s]` is a different duration on every
/// rerun of the same failing test).
fn normalize_signature(lowered: &str) -> String {
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
    truncate_chars(out.trim(), 200)
}
