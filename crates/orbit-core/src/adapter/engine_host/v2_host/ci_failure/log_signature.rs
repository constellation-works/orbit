//! Failed-step log analysis for CI failure filing: the bounded description
//! excerpt, compiler-cause and legacy signatures, and the distinctive anchors
//! a repair brief can quote. Line classification and the normalized error
//! signature belong to CI collection (`orbit_engine::ci_log_signature`), which
//! compares the same identity to decide whether a red run reproduced.

use std::collections::BTreeSet;

use orbit_engine::ci_log_signature::{
    ERROR_MARKERS, LineKind, diagnostic_anchor, is_error_marker_line, is_generic_trailer,
    is_run_command_payload, log_payload, normalize_signature,
};
pub(super) use orbit_engine::ci_log_signature::{
    classify_log_lines, error_signature, is_libtest_stdout_header, signature_payload,
};
use orbit_tools::github_cli::strip_ansi_sequences;
use serde_json::Value;

use super::fields::{truncate_bytes, value_string};

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

/// Lines the description excerpt drops: parameter dumps, group ends, runner
/// bookkeeping and cargo progress.
fn skip_from_excerpt(kind: LineKind) -> bool {
    matches!(
        kind,
        LineKind::ParamDump | LineKind::EndGroup | LineKind::Bookkeeping | LineKind::CargoStatus
    )
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
        .filter(|(_, (kind, _))| !skip_from_excerpt(*kind))
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

/// Where the excerpt names a source file in a compiler or test-panic location
/// (`--> path:line:col`, `panicked at path:line:col`), extract a `file:` context
/// selector for it so CI-sweep tasks are filed with scope.
pub(super) fn extract_context_files_from_log(log: &str) -> Vec<String> {
    let mut files = BTreeSet::new();
    for line in log.lines() {
        if let Some(arrow_idx) = line.find("-->") {
            let after = &line[arrow_idx + 3..];
            for token in after.split_whitespace() {
                if let Some(path) = extract_path_from_location_token(token) {
                    files.insert(format!("file:{path}"));
                    break;
                }
            }
        }
        if let Some(panic_idx) = line.find("panicked at") {
            let after = &line[panic_idx + "panicked at".len()..];
            for token in after.split_whitespace() {
                if let Some(path) = extract_path_from_location_token(token) {
                    files.insert(format!("file:{path}"));
                    break;
                }
            }
        }
    }
    files.into_iter().collect()
}

fn extract_path_from_location_token(token: &str) -> Option<String> {
    let cleaned = strip_ansi_sequences(token);
    let trimmed = cleaned.trim_matches(|c: char| {
        c == '\''
            || c == '"'
            || c == '`'
            || c == ':'
            || c == ','
            || c == '('
            || c == ')'
            || c == '['
            || c == ']'
    });
    let (rest, col_or_line) = trimmed.rsplit_once(':')?;
    if col_or_line.is_empty() || !col_or_line.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let path = if let Some((path_part, line)) = rest.rsplit_once(':') {
        if !line.is_empty() && line.chars().all(|c| c.is_ascii_digit()) {
            path_part
        } else {
            rest
        }
    } else {
        rest
    };
    let normalized = path.replace('\\', "/");
    let path = normalized.strip_prefix("./").unwrap_or(&normalized);
    if path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains("..")
        || path.contains("://")
        || path.starts_with('~')
        || (path.len() >= 2
            && path.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && path.chars().nth(1) == Some(':'))
        || path.starts_with(".cargo/")
        || path.contains("/.cargo/")
        || path.starts_with("target/")
        || path.contains("/target/")
        || path.starts_with("library/std/")
        || path.starts_with("library/core/")
        || path.starts_with("library/alloc/")
        || !is_likely_source_path(path)
    {
        return None;
    }
    Some(path.to_string())
}

fn is_likely_source_path(path: &str) -> bool {
    const SOURCE_EXTENSIONS: &[&str] = &[
        ".rs", ".toml", ".sh", ".c", ".cpp", ".cc", ".h", ".hpp", ".js", ".ts", ".py", ".yaml",
        ".yml", ".json",
    ];
    SOURCE_EXTENSIONS.iter().any(|ext| path.ends_with(ext))
}
