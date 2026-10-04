//! Bounded, redacted preview of a provider's raw stdout capture.

use orbit_agent::latest_assistant_message;
use orbit_common::security::redaction::{PatternRedactor, redact_sensitive_env_text};
use orbit_common::text::{ceil_char_boundary, floor_char_boundary};

pub(super) const STDOUT_TEXT_PREVIEW_LIMIT_BYTES: usize = 64 * 1024;
pub(super) struct StdoutTextPreview {
    pub(super) text: String,
    /// Whether bounding omitted redacted text; capture truncation is separate.
    pub(super) truncated: bool,
    pub(super) preview_bytes: usize,
}

pub(super) fn stdout_text_preview(
    raw: &str,
    redactor: &PatternRedactor,
    prefer_tail: bool,
) -> StdoutTextPreview {
    bounded_redacted_text(raw, redactor, prefer_tail, STDOUT_TEXT_PREVIEW_LIMIT_BYTES)
}

/// Redact `raw` and bound it to `limit` bytes, keeping the head or, with
/// `prefer_tail`, the newest complete lines.
pub(super) fn bounded_redacted_text(
    raw: &str,
    redactor: &PatternRedactor,
    prefer_tail: bool,
    limit: usize,
) -> StdoutTextPreview {
    // Redact the complete capture before selecting a preview. Cutting a raw
    // window can split a secret of any length; a fixed margin cannot protect
    // that fragment when substitutions both expand and shrink the text. The
    // supervisor already bounds the capture independently of this preview.
    let redacted = redactor.apply_str(&redact_sensitive_env_text(raw));
    let truncated = redacted.len() > limit;
    let text = if truncated {
        truncate_preview_text(&redacted, prefer_tail, limit)
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

/// Bound on the final message a step output carries. The complete capture
/// stays addressable through the step's stdout blob reference.
pub(super) const FINAL_MESSAGE_LIMIT_BYTES: usize = 64 * 1024;

/// Bound on the newest message one progress event carries.
pub(super) const PROGRESS_MESSAGE_LIMIT_BYTES: usize = 4 * 1024;

pub(super) struct BoundedMessage {
    pub(super) text: String,
    pub(super) truncated: bool,
    /// Size of the message before it was bounded.
    pub(super) original_bytes: usize,
}

/// The newest assistant message in `stdout`, redacted and cut to `limit`
/// bytes from its start.
pub(super) fn bounded_assistant_message(
    provider: &str,
    stdout: &[u8],
    redactor: &PatternRedactor,
    limit: usize,
) -> Option<BoundedMessage> {
    let message = latest_assistant_message(provider, stdout)?;
    let StdoutTextPreview {
        text, truncated, ..
    } = bounded_redacted_text(&message, redactor, false, limit);
    Some(BoundedMessage {
        text,
        truncated,
        original_bytes: message.len(),
    })
}
