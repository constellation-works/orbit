//! Bounded, redacted preview of a provider's raw stdout capture.

use orbit_common::security::redaction::{PatternRedactor, redact_sensitive_env_text};
use orbit_common::text::{ceil_char_boundary, floor_char_boundary};

pub(super) const STDOUT_TEXT_PREVIEW_LIMIT_BYTES: usize = 64 * 1024;
/// Extra bytes kept around the 64 KiB preview so a secret that straddles the
/// cut is still fully inside the redaction window. After redaction, a windowed
/// source drops this untrusted edge so a split fragment cannot survive when
/// earlier substitutions shrink the text by more than the margin.
const STDOUT_TEXT_PREVIEW_REDACTION_MARGIN_BYTES: usize = 1024;

pub(super) struct StdoutTextPreview {
    pub(super) text: String,
    pub(super) truncated: bool,
    pub(super) preview_bytes: usize,
}

pub(super) fn stdout_text_preview(
    raw: &str,
    redactor: &PatternRedactor,
    prefer_tail: bool,
) -> StdoutTextPreview {
    let limit = STDOUT_TEXT_PREVIEW_LIMIT_BYTES;
    let window = preview_source_window(
        raw,
        prefer_tail,
        limit,
        STDOUT_TEXT_PREVIEW_REDACTION_MARGIN_BYTES,
    );
    let redacted = redactor.apply_str(&redact_sensitive_env_text(window));
    // A secret that straddles the far window edge is split, so the in-window
    // fragment is not a redactor match. Earlier substitutions can shrink the
    // redacted window by more than the margin and pull that fragment inside
    // `limit`. Drop the untrusted raw edge (`window.len() - limit`) from the
    // redacted text whenever the source was larger than the window.
    let source_windowed = raw.len() > window.len();
    let untrusted_edge = window.len().saturating_sub(limit);
    let keep = if source_windowed {
        redacted.len().saturating_sub(untrusted_edge).min(limit)
    } else {
        limit
    };
    let truncated = source_windowed || redacted.len() > keep;
    let text = if redacted.len() > keep {
        truncate_preview_text(&redacted, prefer_tail, keep)
    } else {
        redacted
    };
    let preview_bytes = text.len();

    StdoutTextPreview {
        text,
        truncated,
        preview_bytes,
    }
}

fn preview_source_window(raw: &str, prefer_tail: bool, limit: usize, margin: usize) -> &str {
    let cap = limit.saturating_add(margin);
    if raw.len() <= cap {
        return raw;
    }
    if prefer_tail {
        let requested_start = raw.len() - cap;
        let boundary = ceil_char_boundary(raw, requested_start);
        &raw[boundary..]
    } else {
        let boundary = floor_char_boundary(raw, cap);
        &raw[..boundary]
    }
}

fn truncate_preview_text(redacted: &str, prefer_tail: bool, limit: usize) -> String {
    if prefer_tail {
        let requested_start = redacted.len() - limit;
        let boundary = ceil_char_boundary(redacted, requested_start);
        let line_boundary = redacted[boundary..]
            .find('\n')
            .map_or(boundary, |idx| boundary + idx + 1);
        redacted[line_boundary..].to_string()
    } else {
        let boundary = floor_char_boundary(redacted, limit);
        redacted[..boundary].to_string()
    }
}
