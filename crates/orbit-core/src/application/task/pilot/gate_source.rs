//! Shared source citations and pinned-source refusals for pilot gates.

use std::path::Path;

use orbit_engine::DispatchError;

use super::source::SourceSnapshot;

pub(super) struct GateSource<'a> {
    pub(super) action: &'a str,
    pub(super) task_id: &'a str,
    pub(super) criterion_index: usize,
    pub(super) workspace_root: &'a Path,
    pub(super) source: Option<&'a SourceSnapshot>,
}

pub(super) fn refuse(check: &GateSource<'_>, detail: &str) -> DispatchError {
    DispatchError::action_failed(
        check.action,
        format!(
            "task {} required_os criterion {} {detail}",
            check.task_id, check.criterion_index
        ),
    )
}

pub(super) fn read_pinned_source(
    check: &GateSource<'_>,
    path: &str,
    subject: &str,
    missing_file: &str,
) -> Result<String, DispatchError> {
    let Some(source) = check.source else {
        return Err(refuse(
            check,
            &format!(
                "{subject} cannot be verified without a pinned source; leave required_os empty"
            ),
        ));
    };
    match source.file_text(check.action, check.workspace_root, path)? {
        Some(text) => Ok(text),
        None => Err(refuse(
            check,
            &format!("pinned source has no {missing_file}file at {path}"),
        )),
    }
}

/// Every valid citation in evidence, in order, including repetitions.
pub(super) fn path_line_citations(evidence: &str) -> Vec<(String, usize)> {
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
