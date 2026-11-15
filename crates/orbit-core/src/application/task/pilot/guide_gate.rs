//! Admit a `required_os` entry whose criterion names no OS token only when
//! its evidence cites a repository guide rule that a task tag triggers.
//!
//! The guide is the workspace's own validation contract (for example a
//! `CLAUDE.md`/`AGENTS.md` line requiring a local CodeQL run for a task
//! tagged `security`, with only Linux able to extract every module). Apply
//! reads the cited lines from the pinned source: one cited line must name
//! CodeQL together with a tag the task carries, and another cited line in
//! the same guide must state that only Linux can produce the run. Any other
//! citation stays with the caller's portable-criterion refusal, so a
//! platform mention alone never becomes an OS lock.

use std::path::Path;

use orbit_engine::DispatchError;
use orbit_types::task::HostOs;

use super::source::SourceSnapshot;

/// Whether evidence cites a guide rule this check can admit.
pub(super) enum GuideGateDecision {
    /// Evidence does not name CodeQL, so it cites no guide validation rule.
    NotCited,
    /// The pinned guide confirms a tag-triggered Linux-only validation.
    Admit,
}

/// One `required_os` entry checked against the pinned guide.
pub(super) struct GuideGateCheck<'a> {
    pub(super) action: &'a str,
    pub(super) task_id: &'a str,
    pub(super) criterion_index: usize,
    pub(super) evidence: &'a str,
    pub(super) os: HostOs,
    pub(super) tags: &'a [String],
    pub(super) workspace_root: &'a Path,
    pub(super) source: Option<&'a SourceSnapshot>,
}

/// Guide files that carry a workspace's repository validation rules.
const GUIDE_FILES: &[&str] = &["AGENTS.md", "CLAUDE.md"];

/// Admit `check`, leave it to the caller, or refuse a malformed citation.
pub(super) fn admit_guide_gate(
    check: &GuideGateCheck<'_>,
) -> Result<GuideGateDecision, DispatchError> {
    if !check.evidence.to_ascii_lowercase().contains("codeql") {
        return Ok(GuideGateDecision::NotCited);
    }
    let citations = path_line_citations(check.evidence);
    if citations.is_empty() || citations.iter().any(|(path, _)| !is_guide_path(path)) {
        return Err(refuse(
            check,
            "evidence naming CodeQL must cite the repository guide rule at \
             AGENTS.md:<line> or CLAUDE.md:<line> in the pinned source; leave \
             required_os empty for portable criteria",
        ));
    }
    if citations.iter().any(|(path, _)| path != &citations[0].0) {
        return Err(refuse(
            check,
            "evidence must cite one repository guide file for the CodeQL rule",
        ));
    }
    if check.os != HostOs::Linux {
        return Err(refuse(
            check,
            "a guide rule that only Linux can satisfy must name os linux; \
             leave required_os empty",
        ));
    }
    let (path, _) = &citations[0];
    let text = read_pinned_source(check, path)?;
    let lines: Vec<&str> = text.lines().collect();
    let mut cited = Vec::new();
    for (_, line_no) in &citations {
        let Some(line) = lines.get(line_no - 1) else {
            return Err(refuse(
                check,
                &format!("pinned guide has no line {line_no} at {path}"),
            ));
        };
        cited.push((*line_no, *line));
    }
    let trigger = cited.iter().any(|(_, line)| {
        let lower = line.to_ascii_lowercase();
        lower.contains("codeql")
            && check
                .tags
                .iter()
                .any(|tag| contains_token(&lower, &tag.trim().to_ascii_lowercase()))
    });
    if !trigger {
        return Err(refuse(
            check,
            &format!(
                "no cited guide line at {path} names CodeQL together with a tag this task \
                 carries; leave required_os empty for portable criteria"
            ),
        ));
    }
    let only_linux = cited
        .iter()
        .any(|(_, line)| line.to_ascii_lowercase().contains("only linux"));
    if !only_linux {
        return Err(refuse(
            check,
            &format!(
                "no cited guide line at {path} states that only Linux can produce the \
                 required validation; leave required_os empty for portable criteria"
            ),
        ));
    }
    Ok(GuideGateDecision::Admit)
}

fn refuse(check: &GuideGateCheck<'_>, detail: &str) -> DispatchError {
    DispatchError::action_failed(
        check.action,
        format!(
            "task {} required_os criterion {} {detail}",
            check.task_id, check.criterion_index
        ),
    )
}

fn read_pinned_source(check: &GuideGateCheck<'_>, path: &str) -> Result<String, DispatchError> {
    let Some(source) = check.source else {
        return Err(refuse(
            check,
            "a repository guide rule cannot be verified without a pinned source; leave \
             required_os empty",
        ));
    };
    match source.file_text(check.action, check.workspace_root, path)? {
        Some(text) => Ok(text),
        None => Err(refuse(
            check,
            &format!("pinned source has no guide file at {path}"),
        )),
    }
}

fn is_guide_path(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    GUIDE_FILES.contains(&file)
}

/// Every `path:line` citation in `evidence`, in order, de-duplicated.
fn path_line_citations(evidence: &str) -> Vec<(String, usize)> {
    let bytes = evidence.as_bytes();
    let mut found: Vec<(String, usize)> = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b':' {
            let mut cursor = index + 1;
            if cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
                let mut line = 0usize;
                let mut overflow = false;
                while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
                    let digit = usize::from(bytes[cursor] - b'0');
                    match line
                        .checked_mul(10)
                        .and_then(|value| value.checked_add(digit))
                    {
                        Some(value) => line = value,
                        None => overflow = true,
                    }
                    cursor += 1;
                }
                if !overflow
                    && line >= 1
                    && let Some(path) = path_before(evidence, index)
                    && !found.contains(&(path.clone(), line))
                {
                    found.push((path, line));
                }
                index = cursor;
                continue;
            }
        }
        index += 1;
    }
    found
}

fn path_before(evidence: &str, colon: usize) -> Option<String> {
    let bytes = evidence.as_bytes();
    let mut start = colon;
    while start > 0 && is_path_byte(bytes[start - 1]) {
        start -= 1;
    }
    let path = &evidence[start..colon];
    if valid_source_path(path) {
        Some(path.to_string())
    } else {
        None
    }
}

fn is_path_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/')
}

fn valid_source_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') || path.ends_with('/') || path.contains(':') {
        return false;
    }
    let mut saw_letter = false;
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return false;
        }
        if part.bytes().any(|byte| byte.is_ascii_alphabetic()) {
            saw_letter = true;
        }
    }
    let Some((stem, extension)) = path.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && !extension.is_empty()
        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && saw_letter
}

/// Whether `token` occurs in `text` bounded by non-token bytes. Backticks
/// and punctuation separate, so a quoted tag in a guide line still matches.
fn contains_token(text: &str, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let mut rest = text;
    while let Some(index) = rest.find(token) {
        let end = index + token.len();
        let before = index == 0 || !is_token_byte(rest.as_bytes()[index - 1]);
        let after = end == rest.len() || !is_token_byte(rest.as_bytes()[end]);
        if before && after {
            return true;
        }
        rest = &rest[index + 1..];
    }
    false
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}
