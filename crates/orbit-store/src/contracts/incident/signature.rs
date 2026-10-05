//! Cluster signatures: message normalization that replaces volatile tokens
//! with placeholders, and the stable incident id.

use super::classify::{classify, surface_of};
use chrono::{DateTime, Utc};
use orbit_types::telemetry::AuditEvent;
use std::borrow::Cow;

/// Longest normalized message retained in a signature. Long enough to keep
/// distinct failures distinct, short enough that a multi-kilobyte error body
/// cannot become a grouping key.
const MAX_SIGNATURE_MESSAGE_CHARS: usize = 160;

/// Replaces volatile tokens in an error message so that the same failure over
/// different operands collapses to one signature.
///
/// The rules are shape-based only — no vocabulary from any project, tool, or
/// agent appears here.
pub fn normalize_message(raw: &str) -> String {
    let mut out = String::new();
    for token in raw.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&normalize_token(token));
        if out.chars().count() >= MAX_SIGNATURE_MESSAGE_CHARS {
            break;
        }
    }
    if out.chars().count() > MAX_SIGNATURE_MESSAGE_CHARS {
        out = out.chars().take(MAX_SIGNATURE_MESSAGE_CHARS).collect();
    }
    out
}

fn normalize_token(token: &str) -> Cow<'_, str> {
    let trimmed = token.trim_matches(['"', '\'', '`', '(', ')', ',']);
    if trimmed.is_empty() {
        return Cow::Borrowed(token);
    }
    // Order matters: the most specific shapes are tested first so a token that
    // satisfies several rules gets its most informative placeholder.
    if looks_like_timestamp(trimmed) {
        return Cow::Borrowed("<ts>");
    }
    if looks_like_uuid(trimmed) {
        return Cow::Borrowed("<uuid>");
    }
    if looks_like_prefixed_id(trimmed) {
        return Cow::Borrowed("<id>");
    }
    if looks_like_path(trimmed) {
        return Cow::Borrowed("<path>");
    }
    if looks_like_hex(trimmed) {
        return Cow::Borrowed("<hex>");
    }
    if looks_like_number(trimmed) {
        return Cow::Borrowed("<num>");
    }
    Cow::Owned(trimmed.to_string())
}

/// `2026-08-15T06:34:00Z` and friends: leading four digits, a `-`, and a
/// digit-dominated body.
fn looks_like_timestamp(token: &str) -> bool {
    let bytes = token.as_bytes();
    bytes.len() >= 10
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && token.chars().filter(char::is_ascii_digit).count() >= 8
}

fn looks_like_uuid(token: &str) -> bool {
    let groups: Vec<&str> = token.split('-').collect();
    groups.len() == 5
        && [8_usize, 4, 4, 4, 12]
            .iter()
            .zip(&groups)
            .all(|(len, group)| group.len() == *len && group.chars().all(|c| c.is_ascii_hexdigit()))
}

/// `ABC-1234`, `T20260428-7`, `jrun-20260816-0634-8`: an alphanumeric stem
/// joined to a digit run. Covers generated record ids of any project without
/// naming one.
fn looks_like_prefixed_id(token: &str) -> bool {
    let Some((head, tail)) = token.split_once('-') else {
        return false;
    };
    if head.is_empty() || !head.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    if !head.chars().any(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let rest: String = tail.chars().filter(|c| *c != '-').collect();
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}

fn looks_like_path(token: &str) -> bool {
    token.contains('/') || token.starts_with('~') || token.contains('\\')
}

fn looks_like_hex(token: &str) -> bool {
    token.len() >= 8
        && token.chars().all(|c| c.is_ascii_hexdigit())
        && token.chars().any(|c| c.is_ascii_alphabetic())
}

fn looks_like_number(token: &str) -> bool {
    let stripped = token.trim_end_matches(['.', ':', ';']);
    !stripped.is_empty()
        && stripped
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '_' | '-' | '+'))
        && stripped.chars().any(|c| c.is_ascii_digit())
}

/// The grouping signature for one failed row: class, actor, surface, and the
/// normalized message. Rendered as a readable string so the UI can show the
/// operator exactly what was collapsed.
pub fn signature_for(event: &AuditEvent) -> String {
    let class = classify(event);
    let message = normalize_message(event.error_message.as_deref().unwrap_or_default());
    format!(
        "{}|role={}|surface={}|msg={}",
        class.as_str(),
        if event.role.is_empty() {
            "unknown"
        } else {
            event.role.as_str()
        },
        surface_of(event),
        if message.is_empty() {
            format!("exit={}", event.exit_code)
        } else {
            message
        }
    )
}

/// Deterministic 64-bit FNV-1a over the grouping key, rendered as hex. Used
/// only as a stable client-side handle for an incident — never persisted, and
/// never a security boundary.
pub(super) fn incident_id(run_scope: &str, signature: &str, first_ts: DateTime<Utc>) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let key = format!(
        "{run_scope}\u{1f}{signature}\u{1f}{}",
        first_ts.to_rfc3339()
    );
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("inc-{hash:016x}")
}
