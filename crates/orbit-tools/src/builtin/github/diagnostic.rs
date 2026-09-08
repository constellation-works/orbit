//! Bounded evidence from one uniquely completed failing runner command.
//!
//! Small commands are retained whole. Oversized commands may instead supply
//! failure regions: all recognized failure anchors and their following context,
//! with explicit byte omissions. Neither representation proves job/step identity;
//! the caller still binds the evidence to the provider's failed-job metadata.

use orbit_common::security::redaction::redact_all;
use serde_json::{Value, json};

use super::MAX_CHECKOUT_LOG_SCAN_BYTES;

const MAX_UNIT_BYTES: usize = 262_144;
const MAX_LINE_BYTES: usize = 16_384;
const MAX_REGION_BYTES: usize = 65_536;
const ASSERTION_PREFIX_BYTES: usize = 512;
const CONTEXT_LINES: usize = 12;

#[derive(Default)]
pub(super) struct DiagnosticCollector {
    scanned: usize,
    line: Vec<u8>,
    line_bytes: usize,
    invalid: bool,
    candidate: Option<Command>,
    selected: Option<Command>,
    failures: usize,
}

#[derive(Default)]
struct Command {
    full: Option<String>,
    regions: String,
    total_bytes: usize,
    retained_bytes: usize,
    assertion_omitted_bytes: usize,
    reported_gap_bytes: usize,
    anchors: usize,
    context_left: usize,
    regions_overflow: bool,
    columns: Option<String>,
    conflicting_columns: bool,
}

impl DiagnosticCollector {
    pub(super) fn push(&mut self, chunk: &[u8]) {
        self.scanned = self.scanned.saturating_add(chunk.len());
        if self.scanned > MAX_CHECKOUT_LOG_SCAN_BYTES {
            self.invalid = true;
        }
        if self.invalid {
            return;
        }
        for byte in chunk {
            self.line_bytes += 1;
            if self.line.len() < MAX_LINE_BYTES {
                self.line.push(*byte);
            }
            if *byte == b'\n' {
                let bytes = std::mem::take(&mut self.line);
                let total_bytes = std::mem::take(&mut self.line_bytes);
                // A retained prefix may end inside a UTF-8 character. Only
                // assertion payloads are allowed to exceed the line buffer.
                let line = match std::str::from_utf8(&bytes) {
                    Ok(line) => line,
                    Err(error) if error.error_len().is_none() && total_bytes > bytes.len() => {
                        std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap_or_default()
                    }
                    Err(_) => {
                        self.invalid = true;
                        return;
                    }
                };
                self.observe(line, total_bytes);
            }
        }
    }

    fn observe(&mut self, line: &str, total_bytes: usize) {
        let (columns, payload) = runner_payload(line);
        let plain = strip_ansi_sequences(payload);
        let payload = plain.trim();
        let lower = payload.to_ascii_lowercase();
        let source_notice = payload.starts_with("##[warning]")
            || payload.starts_with("##[error]")
            || payload.starts_with("Log output")
            || payload.starts_with("[...");
        if source_notice
            && lower.contains("log")
            && (lower.contains("truncat") || lower.contains("omitted"))
        {
            self.invalid = true;
        }
        let assertion_payload = payload.starts_with("left:") || payload.starts_with("right:");
        if total_bytes > line.len() && !assertion_payload {
            self.invalid = true;
        }
        let start = payload.starts_with("##[group]Run ");
        if start {
            self.candidate = Some(Command {
                full: Some(String::new()),
                columns: columns.map(ToOwned::to_owned),
                ..Command::default()
            });
        }
        let completed = payload
            .strip_prefix("##[error]Process completed with exit code ")
            .and_then(|code| code.strip_suffix('.'))
            .and_then(|code| code.parse::<i32>().ok())
            .is_some_and(|code| code != 0);
        if let Some(candidate) = &mut self.candidate {
            candidate.observe(line, total_bytes, payload, start || completed);
        }
        if completed {
            self.failures += 1;
            self.selected = self.candidate.take();
        }
    }

    pub(super) fn source_complete(&self) -> bool {
        !self.invalid && self.line_bytes == 0
    }

    pub(super) fn finish(self) -> (Option<String>, Option<Value>) {
        if !self.source_complete() || self.failures != 1 {
            return (None, None);
        }
        let Some(command) = self.selected.filter(|command| !command.conflicting_columns) else {
            return (None, None);
        };
        if let Some(full) = command.full {
            return (Some(redact_all(&full)), None);
        }
        if command.regions_overflow || command.anchors == 0 {
            return (None, None);
        }
        let text = redact_all(&command.regions);
        if text.len() > MAX_REGION_BYTES {
            return (None, None);
        }
        let regions = json!({
            "kind": "runner_failure_regions",
            "complete": false,
            "command_complete": true,
            "selection_complete": true,
            "text": text,
            "returned_bytes": text.len(),
            "command_bytes": command.total_bytes,
            "retained_source_bytes": command.retained_bytes,
            "omitted_bytes": command.total_bytes - command.retained_bytes,
            "assertion_payload_omitted_bytes": command.assertion_omitted_bytes,
            "failure_anchor_count": command.anchors,
        });
        (None, Some(regions))
    }
}

impl Command {
    fn observe(&mut self, line: &str, total_bytes: usize, payload: &str, boundary: bool) {
        let columns = runner_payload(line).0;
        let assertion_payload = payload.starts_with("left:") || payload.starts_with("right:");
        self.total_bytes += total_bytes;
        self.conflicting_columns |= self.columns.as_deref() != columns;
        if let Some(full) = &mut self.full {
            if full.len() + total_bytes > MAX_UNIT_BYTES || total_bytes != line.len() {
                self.full = None;
            } else {
                full.push_str(line);
            }
        }

        // Anchored forms avoid passing tests whose names contain "failure".
        // Keep every anchor, not just the last failure in a multi-test command.
        let anchor = payload.starts_with("FAIL ")
            || (payload.starts_with("test ") && payload.ends_with(" ... FAILED"))
            || (payload.starts_with("thread '") && payload.contains(" panicked at "))
            || payload.starts_with("error[")
            || payload.starts_with("error:")
            || payload.starts_with("assertion ");
        if anchor {
            self.anchors += 1;
            self.context_left = CONTEXT_LINES;
        }
        let summary = payload.starts_with("Summary ") || payload.starts_with("test result: FAILED");
        let retain = boundary || anchor || summary || self.context_left > 0;
        self.context_left = self.context_left.saturating_sub(1);
        if !retain || self.regions_overflow {
            return;
        }
        let gap = self.total_bytes
            - total_bytes
            - self.retained_bytes
            - self.assertion_omitted_bytes
            - self.reported_gap_bytes;
        let gap_marker = if gap > 0 {
            self.reported_gap_bytes += gap;
            format!(
                "{}[... {gap} command bytes omitted ...]\n",
                columns.unwrap_or_default()
            )
        } else {
            String::new()
        };
        let (text, retained) = if assertion_payload && total_bytes > ASSERTION_PREFIX_BYTES {
            let mut end = ASSERTION_PREFIX_BYTES.min(line.len());
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            // Never cut a secret-shaped token before redaction can recognize
            // it. Drop the final partial whitespace-delimited token instead.
            if !line[..end].ends_with(char::is_whitespace) {
                end = line[..end]
                    .char_indices()
                    .rfind(|(_, ch)| ch.is_whitespace())
                    .map_or(0, |(index, ch)| index + ch.len_utf8());
            }
            let omitted = total_bytes - end;
            self.assertion_omitted_bytes += omitted;
            (
                format!(
                    "{} [... {omitted} assertion payload bytes omitted ...]\n",
                    &line[..end]
                ),
                end,
            )
        } else {
            (line.to_string(), total_bytes)
        };
        if self.regions.len() + gap_marker.len() + text.len() > MAX_REGION_BYTES {
            self.regions_overflow = true;
            self.regions.clear();
            return;
        }
        self.regions.push_str(&gap_marker);
        self.regions.push_str(&text);
        self.retained_bytes += retained;
    }
}

fn runner_payload(line: &str) -> (Option<&str>, &str) {
    // Raw job API logs start with a timestamp. In gh's display format only
    // the first two tabs delimit job/step; tabs inside assertions are data.
    if let Some((_, payload)) = timestamp_payload(line) {
        return (None, payload);
    }
    let mut columns = line.splitn(3, '\t');
    let _ = columns.next();
    let _ = columns.next();
    if let Some(payload) = columns.next() {
        let prefix = &line[..line.len() - payload.len()];
        return (
            Some(prefix),
            timestamp_payload(payload).map_or(payload, |(_, rest)| rest),
        );
    }
    (None, line)
}

fn timestamp_payload(text: &str) -> Option<(&str, &str)> {
    text.split_once(' ').filter(|(prefix, _)| {
        prefix.ends_with('Z') && prefix.contains('T') && !prefix.contains('\t')
    })
}

/// CSI/OSC sequences only. The raw log line remains in the task description.
///
/// Walks characters rather than bytes: a truncated or malformed escape in a
/// runner log must not split a multi-byte character.
pub fn strip_ansi_sequences(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            // CSI: parameters and intermediates, then one final character.
            Some('[') => {
                for ch in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&ch) {
                        break;
                    }
                }
            }
            // OSC: runs to BEL or the ST terminator.
            Some(']') => {
                while let Some(ch) = chars.next() {
                    if ch == '\u{07}' {
                        break;
                    }
                    if ch == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            // A lone escape, or a two-character sequence: drop both.
            _ => {}
        }
    }
    out
}
