//! Admit a `required_os` entry whose criterion names no OS token only when
//! its evidence cites one verified `#[cfg(target_os = "<os>")]` gate.
//!
//! The citation is a single attribute, one repository-relative `path:line`,
//! and the item that attribute gates. Apply reads that line from the pinned
//! source, not the working tree. `cfg(unix)`, `cfg(not(windows))` and any
//! other attribute that does not name exactly one of linux, macos or windows
//! stay refused.

use std::path::Path;

use orbit_engine::DispatchError;
use orbit_types::task::HostOs;

use super::source::SourceSnapshot;

/// Whether evidence cites a cfg gate this check can admit.
pub(super) enum CfgGateDecision {
    /// Evidence cites no `#[cfg(...)]` attribute.
    NotCited,
    /// The pinned source confirms the gate on an item the criterion names.
    Admit,
}

/// One `required_os` entry checked against the pinned source.
pub(super) struct CfgGateCheck<'a> {
    pub(super) action: &'a str,
    pub(super) task_id: &'a str,
    pub(super) criterion_index: usize,
    pub(super) criterion: &'a str,
    pub(super) evidence: &'a str,
    pub(super) os: HostOs,
    pub(super) workspace_root: &'a Path,
    pub(super) source: Option<&'a SourceSnapshot>,
}

/// Admit `check` or refuse it. [`CfgGateDecision::NotCited`] leaves the
/// portable-criterion refusal to the caller, whose wording is already pinned.
pub(super) fn admit_cfg_gate(check: &CfgGateCheck<'_>) -> Result<CfgGateDecision, DispatchError> {
    let attributes = cfg_attributes(check.evidence);
    if attributes.is_empty() {
        return Ok(CfgGateDecision::NotCited);
    }
    let Some(gated) = single_determining_os(&attributes) else {
        return Err(refuse(
            check,
            "evidence cites a cfg gate that does not determine one of linux, macos, or windows; \
             leave required_os empty",
        ));
    };
    let Some((path, line_no)) = single_path_line(check.evidence) else {
        return Err(refuse(
            check,
            "evidence must cite the cfg gate at one repository-relative path:line",
        ));
    };
    let text = read_pinned_source(check, &path)?;
    let lines: Vec<&str> = text.lines().collect();
    let source_line = lines.get(line_no - 1).copied().unwrap_or("");
    let Some(source_os) = exact_attribute_os(source_line) else {
        return Err(refuse(
            check,
            &format!(
                "evidence cites #[cfg(target_os = \"{gated}\")] at {path}:{line_no}, but the \
                 pinned source does not have that gate there"
            ),
        ));
    };
    if source_os != gated {
        return Err(refuse(
            check,
            &format!(
                "evidence cites #[cfg(target_os = \"{gated}\")] at {path}:{line_no}, but the \
                 pinned source gate is target_os = \"{source_os}\""
            ),
        ));
    }
    let Some(item) = item_after_gate(&lines, line_no - 1) else {
        return Err(refuse(
            check,
            &format!("the cfg gate at {path}:{line_no} does not apply to a named item"),
        ));
    };
    if !names_token(check.criterion, &item) {
        return Err(refuse(
            check,
            &format!(
                "does not name the item `{item}` gated at {path}:{line_no}; leave required_os \
                 empty"
            ),
        ));
    }
    if check.os != gated {
        return Err(refuse(
            check,
            &format!(
                "os is {os} but the cited gate is target_os = \"{gated}\"; leave required_os empty",
                os = check.os
            ),
        ));
    }
    Ok(CfgGateDecision::Admit)
}

fn refuse(check: &CfgGateCheck<'_>, detail: &str) -> DispatchError {
    DispatchError::action_failed(
        check.action,
        format!(
            "task {} required_os criterion {} {detail}",
            check.task_id, check.criterion_index
        ),
    )
}

fn read_pinned_source(check: &CfgGateCheck<'_>, path: &str) -> Result<String, DispatchError> {
    let Some(source) = check.source else {
        return Err(refuse(
            check,
            "a cfg gate cannot be verified without a pinned source; leave required_os empty",
        ));
    };
    match source.file_text(check.action, check.workspace_root, path)? {
        Some(text) => Ok(text),
        None => Err(refuse(
            check,
            &format!("pinned source has no file at {path}"),
        )),
    }
}

/// `Some(os)` when `#[cfg(...)]` is exactly `target_os = "<os>"`. `None` is a
/// cfg attribute that does not determine one OS.
fn cfg_attributes(evidence: &str) -> Vec<Option<HostOs>> {
    let mut found = Vec::new();
    let mut offset = 0;
    while let Some(relative) = evidence[offset..].find("#[") {
        let start = offset + relative;
        if let Some((os, consumed)) = parse_cfg_attribute(&evidence[start..]) {
            found.push(os);
            offset = start + consumed.max(2);
        } else {
            offset = start + 2;
        }
    }
    found
}

fn single_determining_os(attributes: &[Option<HostOs>]) -> Option<HostOs> {
    let mut gated = None;
    for attribute in attributes {
        let os = (*attribute)?;
        if gated.is_some_and(|existing| existing != os) {
            return None;
        }
        gated = Some(os);
    }
    gated
}

/// Parse a `#[cfg(...)]` at the start of `text`. The returned length is the
/// span consumed from `text`.
fn parse_cfg_attribute(text: &str) -> Option<(Option<HostOs>, usize)> {
    if !text.starts_with("#[") {
        return None;
    }
    let mut index = skip_ascii_ws(text, 2);
    let keyword = "cfg";
    if !text[index..].starts_with(keyword) {
        return None;
    }
    index += keyword.len();
    if text
        .as_bytes()
        .get(index)
        .is_some_and(|byte| is_ident_byte(*byte))
    {
        return None;
    }
    index = skip_ascii_ws(text, index);
    if text.as_bytes().get(index) != Some(&b'(') {
        return None;
    }
    index += 1;
    let args_start = index;
    let args_end = end_of_balanced_paren(text, index)?;
    let args = text.get(args_start..args_end)?;
    index = skip_ascii_ws(text, args_end + 1);
    if text.as_bytes().get(index) != Some(&b']') {
        return None;
    }
    Some((determining_os(args), index + 1))
}

fn determining_os(args: &str) -> Option<HostOs> {
    let mut index = skip_ascii_ws(args, 0);
    let key = "target_os";
    if !args[index..].starts_with(key) {
        return None;
    }
    index += key.len();
    if args
        .as_bytes()
        .get(index)
        .is_some_and(|byte| is_ident_byte(*byte))
    {
        return None;
    }
    index = skip_ascii_ws(args, index);
    if args.as_bytes().get(index) != Some(&b'=') {
        return None;
    }
    index = skip_ascii_ws(args, index + 1);
    if args.as_bytes().get(index) != Some(&b'"') {
        return None;
    }
    index += 1;
    let name_start = index;
    let name_end = args[index..].find('"')? + index;
    let name = &args[name_start..name_end];
    index = skip_ascii_ws(args, name_end + 1);
    if index != args.len() {
        return None;
    }
    HostOs::parse(name)
}

fn exact_attribute_os(line: &str) -> Option<HostOs> {
    let line = line.trim();
    let (os, consumed) = parse_cfg_attribute(line)?;
    if consumed != line.len() { None } else { os }
}

fn single_path_line(evidence: &str) -> Option<(String, usize)> {
    let bytes = evidence.as_bytes();
    let mut found = None;
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
                    if found.is_some() {
                        return None;
                    }
                    found = Some((path, line));
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

/// How many source lines after the attribute may hold doc comments and
/// further attributes before the gated item. Past that the citation is
/// unverified.
const ITEM_SCAN_LINES: usize = 40;

fn item_after_gate(lines: &[&str], cfg_index: usize) -> Option<String> {
    for line in lines.iter().skip(cfg_index + 1).take(ITEM_SCAN_LINES) {
        let trimmed = line.trim();
        if skippable_between_gate_and_item(trimmed) {
            continue;
        }
        return definition_name(trimmed);
    }
    None
}

fn skippable_between_gate_and_item(line: &str) -> bool {
    if line.is_empty() || line.starts_with("//") || line.starts_with('#') {
        return true;
    }
    line.starts_with("/*") && line.contains("*/")
}

fn definition_name(line: &str) -> Option<String> {
    let rest = strip_qualifiers(line);
    let keyword = ident_prefix(rest)?;
    let after = rest[keyword.len()..].trim_start();
    match keyword {
        "fn" | "mod" | "struct" | "enum" | "trait" | "type" | "static" | "union" | "const" => {
            let name = ident_prefix(after)?;
            Some(name.to_string())
        }
        _ => None,
    }
}

fn strip_qualifiers(line: &str) -> &str {
    let mut rest = line.trim_start();
    for _ in 0..8 {
        if let Some(after) = strip_visibility(rest) {
            rest = after;
            continue;
        }
        if let Some(after) = strip_word(rest, "async").or_else(|| strip_word(rest, "unsafe")) {
            rest = after;
            continue;
        }
        if let Some(after) = strip_word(rest, "extern") {
            rest = skip_optional_abi(after);
            continue;
        }
        if let Some(after) = strip_word(rest, "const")
            && strip_word(after, "fn").is_some()
        {
            rest = after;
            continue;
        }
        break;
    }
    rest
}

fn strip_visibility(text: &str) -> Option<&str> {
    let after = strip_word(text, "pub")?;
    if let Some(group) = after.strip_prefix('(') {
        let end = group.find(')')?;
        return Some(group[end + 1..].trim_start());
    }
    Some(after)
}

fn skip_optional_abi(text: &str) -> &str {
    let text = text.trim_start();
    if let Some(rest) = text.strip_prefix('"')
        && let Some(end) = rest.find('"')
    {
        return rest[end + 1..].trim_start();
    }
    text
}

fn strip_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let text = text.trim_start();
    let rest = text.strip_prefix(word)?;
    if rest.starts_with(is_ident_char) {
        return None;
    }
    Some(rest.trim_start())
}

fn ident_prefix(text: &str) -> Option<&str> {
    let mut end = 0;
    for (index, ch) in text.char_indices() {
        if !is_ident_char(ch) || (index == 0 && ch.is_ascii_digit()) {
            break;
        }
        end = index + ch.len_utf8();
    }
    if end == 0 { None } else { Some(&text[..end]) }
}

fn names_token(text: &str, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let mut rest = text;
    while let Some(index) = rest.find(token) {
        let end = index + token.len();
        let before = index == 0 || !is_ident_byte(rest.as_bytes()[index - 1]);
        let after = end == rest.len() || !is_ident_byte(rest.as_bytes()[end]);
        if before && after {
            return true;
        }
        rest = &rest[index + 1..];
    }
    false
}

fn end_of_balanced_paren(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 1usize;
    let mut index = start;
    let mut in_string = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if byte == b'\\' {
                index = index.saturating_add(2);
                continue;
            }
            if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn skip_ascii_ws(text: &str, mut index: usize) -> usize {
    let bytes = text.as_bytes();
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    index
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_ident_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}
