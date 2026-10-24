//! The one projection of a run's CLI invocation records.
//!
//! `orbit run logs`, the dashboard's run-log API and the bounded MCP run
//! detail each select a [`RunCliInvocationProfile`]. The profile owns which
//! fields a record carries, how much of each stream blob is read, and the
//! order in which a preview is truncated and redacted. Each surface reads its
//! records through the existing collectors with the profile's
//! [`RunCliInvocationProfile::blob_read_max_bytes`] and renders only its own
//! envelope around [`RunCliInvocationRecord::to_profile_json`].

use orbit_common::security::redaction::redact_all;
use serde_json::{Value, json};

use super::run::RunCliInvocationRecord;

/// Bytes of each stream the HTTP preview reads and keeps.
pub const HTTP_PREVIEW_MAX_BYTES: usize = 8192;
/// Lines of each stream the HTTP preview keeps.
const HTTP_PREVIEW_MAX_LINES: usize = 120;
/// Bytes of each stream the bounded excerpt reads and keeps.
pub const BOUNDED_EXCERPT_MAX_BYTES: usize = 4096;

/// A named projection of [`RunCliInvocationRecord`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunCliInvocationProfile {
    /// `orbit run logs`: step, provider, blob refs and the full captured
    /// streams, with no truncation flags.
    Raw,
    /// The dashboard run-log API: every record field, streams as
    /// `stdout_preview`/`stderr_preview` cut to 8192 bytes or 120 lines and
    /// then redacted.
    HttpPreview,
    /// The bounded MCP run detail: no run id or blob refs, streams redacted
    /// and then cut to 4096 bytes.
    BoundedExcerpt,
}

impl RunCliInvocationProfile {
    /// Bytes of each stream blob the collector should read; `None` reads the
    /// whole capture. A preview profile never asks for the raw capture.
    pub const fn blob_read_max_bytes(self) -> Option<usize> {
        match self {
            Self::Raw => None,
            Self::HttpPreview => Some(HTTP_PREVIEW_MAX_BYTES),
            Self::BoundedExcerpt => Some(BOUNDED_EXCERPT_MAX_BYTES),
        }
    }
}

impl RunCliInvocationRecord {
    /// This record as `profile` presents it. A stream is truncated when the
    /// profile cut it or the collector's blob read stopped before its end.
    pub fn to_profile_json(&self, profile: RunCliInvocationProfile) -> Value {
        match profile {
            RunCliInvocationProfile::Raw => json!({
                "step_id": self.step_id,
                "provider": self.provider,
                "stdout_blob_ref": self.stdout_blob_ref,
                "stderr_blob_ref": self.stderr_blob_ref,
                "stdout": self.stdout,
                "stderr": self.stderr,
            }),
            RunCliInvocationProfile::HttpPreview => {
                let (stdout, stdout_truncated) = http_preview(&self.stdout);
                let (stderr, stderr_truncated) = http_preview(&self.stderr);
                json!({
                    "run_id": self.run_id,
                    "event_id": self.event_id,
                    "ts": self.ts.map(|ts| ts.to_rfc3339()),
                    "step_id": self.step_id,
                    "step_index": self.step_index,
                    "provider": self.provider,
                    "stdout_blob_ref": self.stdout_blob_ref,
                    "stderr_blob_ref": self.stderr_blob_ref,
                    "stdout_preview": stdout,
                    "stderr_preview": stderr,
                    "stdout_truncated": stdout_truncated || self.stdout_blob_truncated,
                    "stderr_truncated": stderr_truncated || self.stderr_blob_truncated,
                    "exit_code": self.exit_code,
                    "timed_out": self.timed_out,
                    "duration_ms": self.duration_ms,
                })
            }
            RunCliInvocationProfile::BoundedExcerpt => {
                let (stdout, stdout_truncated) =
                    bounded_redacted_text(&self.stdout, BOUNDED_EXCERPT_MAX_BYTES);
                let (stderr, stderr_truncated) =
                    bounded_redacted_text(&self.stderr, BOUNDED_EXCERPT_MAX_BYTES);
                json!({
                    "event_id": self.event_id,
                    "ts": self.ts,
                    "step_id": self.step_id,
                    "step_index": self.step_index,
                    "provider": self.provider,
                    "stdout": stdout,
                    "stderr": stderr,
                    "stdout_truncated": self.stdout_blob_truncated || stdout_truncated,
                    "stderr_truncated": self.stderr_blob_truncated || stderr_truncated,
                    "exit_code": self.exit_code,
                    "timed_out": self.timed_out,
                    "duration_ms": self.duration_ms,
                })
            }
        }
    }
}

/// Whole lines up to the HTTP byte and line budgets (a first line longer than
/// the byte budget is cut at a character boundary), redacted after the cut.
fn http_preview(raw: &str) -> (String, bool) {
    let mut out = String::new();
    let mut truncated = false;
    for (index, line) in raw.lines().enumerate() {
        if index >= HTTP_PREVIEW_MAX_LINES {
            truncated = true;
            break;
        }
        let needed = line.len() + usize::from(!out.is_empty());
        if out.len().saturating_add(needed) > HTTP_PREVIEW_MAX_BYTES {
            if out.is_empty() {
                for ch in line.chars() {
                    if out.len().saturating_add(ch.len_utf8()) > HTTP_PREVIEW_MAX_BYTES {
                        break;
                    }
                    out.push(ch);
                }
            }
            truncated = true;
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    if raw.ends_with('\n') && !out.is_empty() && out.len() < HTTP_PREVIEW_MAX_BYTES {
        out.push('\n');
    }
    (redact_all(&out), truncated)
}

/// `raw` redacted, then cut to at most `max_bytes` at a character boundary,
/// with whether the cut removed anything. The bounded excerpt's stream policy,
/// which the desktop projections also apply to other operator text.
pub fn bounded_redacted_text(raw: &str, max_bytes: usize) -> (String, bool) {
    let mut text = redact_all(raw);
    if text.len() <= max_bytes {
        return (text, false);
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
}
