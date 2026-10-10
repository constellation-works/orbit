//! Bounded evidence from one uniquely completed failing runner command.
//!
//! Small commands are retained whole. Oversized commands may instead supply
//! failure regions: recognized failure anchors and the output block that
//! explains each failing test, with explicit byte omissions. A block runs from
//! the anchor through captured stdout/stderr until the next test status line,
//! the captured-output section end, or the summary. The region budget can cut
//! that block; the cut is marked so it is not an ordinary gap. Compiler anchors
//! keep a short following window. Neither representation proves job/step
//! identity; the caller still binds the evidence to the provider's failed-job
//! metadata.

use orbit_common::security::redaction::redact_all;
use orbit_common::text::floor_char_boundary;
use serde_json::{Value, json};

use super::MAX_CHECKOUT_LOG_SCAN_BYTES;

const MAX_UNIT_BYTES: usize = 262_144;
const MAX_LINE_BYTES: usize = 16_384;
const MAX_REGION_BYTES: usize = 65_536;
const ASSERTION_PREFIX_BYTES: usize = 512;
const CONTEXT_LINES: usize = 12;
/// Room for the cut marker plus the status, summary, and process-exit lines
/// that must still be retained after a failure block stops early.
const BLOCK_TAIL_RESERVE: usize = 1024;

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
    /// Captured output of a failing test is open. Keep it until the block
    /// ends, rather than for [`CONTEXT_LINES`] only.
    block_open: bool,
    /// The open block was cut so the selection stays inside the region budget.
    block_cut: bool,
    seen_test_failure: bool,
    in_captured_section: bool,
    /// Source bytes dropped from a cut block and not yet named by a marker.
    cut_unreported: usize,
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
        let anchor = is_failure_anchor(payload);
        let test_anchor = is_test_block_anchor(payload);
        if anchor {
            self.anchors += 1;
        }
        if test_anchor {
            self.seen_test_failure = true;
            self.block_open = true;
            self.block_cut = false;
            self.context_left = 0;
        } else if anchor && !self.block_open {
            self.context_left = CONTEXT_LINES;
        }
        if is_captured_section_header(payload) {
            self.in_captured_section = true;
            if self.seen_test_failure && !self.block_cut {
                self.block_open = true;
                self.context_left = 0;
            }
        }
        if is_nextest_status_line(payload) {
            self.in_captured_section = false;
        }
        let summary = payload.starts_with("Summary ") || payload.starts_with("test result: FAILED");
        let ends_block = self.ends_open_block(payload, summary, boundary);
        let window = !self.block_open && self.context_left > 0;
        let retain = boundary || anchor || summary || self.block_open || window;
        if !self.block_open {
            self.context_left = self.context_left.saturating_sub(1);
        }
        // Payload inside an open block can be cut. Anchors, the short compiler
        // window, and the line that ends the block cannot: dropping those
        // either hides a later failure or pretends the block completed.
        let must_keep = boundary || anchor || summary || window || ends_block;
        let block_payload = self.block_open && !must_keep;
        if self.regions_overflow || !retain {
            return;
        }
        if self.block_cut && block_payload {
            self.cut_unreported = self.cut_unreported.saturating_add(total_bytes);
            return;
        }
        let gap = self.total_bytes
            - total_bytes
            - self.retained_bytes
            - self.assertion_omitted_bytes
            - self.reported_gap_bytes;
        let cut_part = self.cut_unreported.min(gap);
        let ordinary = gap - cut_part;
        let cut_marker = if cut_part > 0 {
            output_block_cut_marker(columns, cut_part)
        } else {
            String::new()
        };
        let gap_marker = if ordinary > 0 {
            format!(
                "{}[... {ordinary} command bytes omitted ...]\n",
                columns.unwrap_or_default()
            )
        } else {
            String::new()
        };
        let (text, retained, assertion_omitted) =
            if assertion_payload && total_bytes > ASSERTION_PREFIX_BYTES {
                let mut end = floor_char_boundary(line, ASSERTION_PREFIX_BYTES);
                // Never cut a secret-shaped token before redaction can recognize
                // it. Drop the final partial whitespace-delimited token instead.
                if !line[..end].ends_with(char::is_whitespace) {
                    end = line[..end]
                        .char_indices()
                        .rfind(|(_, ch)| ch.is_whitespace())
                        .map_or(0, |(index, ch)| index + ch.len_utf8());
                }
                let omitted = total_bytes - end;
                (
                    format!(
                        "{} [... {omitted} assertion payload bytes omitted ...]\n",
                        &line[..end]
                    ),
                    end,
                    omitted,
                )
            } else {
                (line.to_string(), total_bytes, 0)
            };
        let addition = cut_marker.len() + gap_marker.len() + text.len();
        // Leave room to name a later cut and still keep the block's ending
        // lines. Required lines use the real cap and withhold the selection
        // when they themselves do not fit.
        let limit = if block_payload {
            MAX_REGION_BYTES.saturating_sub(BLOCK_TAIL_RESERVE)
        } else {
            MAX_REGION_BYTES
        };
        if self.regions.len() + addition > limit {
            if block_payload {
                // Line boundary: the omitted line is whole, so a secret on it
                // is not split into the retained text. `redact_all` still
                // covers every token that was kept.
                self.block_cut = true;
                self.cut_unreported = self.cut_unreported.saturating_add(total_bytes);
                return;
            }
            self.regions_overflow = true;
            self.regions.clear();
            return;
        }
        self.reported_gap_bytes += cut_part + ordinary;
        self.cut_unreported -= cut_part;
        self.assertion_omitted_bytes += assertion_omitted;
        self.regions.push_str(&cut_marker);
        self.regions.push_str(&gap_marker);
        self.regions.push_str(&text);
        self.retained_bytes += retained;
        if ends_block && !test_anchor {
            self.block_open = false;
            self.block_cut = false;
            self.in_captured_section = false;
        }
    }

    fn ends_open_block(&self, payload: &str, summary: bool, boundary: bool) -> bool {
        if !self.block_open {
            return false;
        }
        if boundary || summary || is_section_rule(payload) {
            return true;
        }
        if is_nextest_status_line(payload) && !is_test_block_anchor(payload) {
            return true;
        }
        is_libtest_status_line(payload)
            && !is_test_block_anchor(payload)
            && !self.in_captured_section
    }
}

fn is_failure_anchor(payload: &str) -> bool {
    payload.starts_with("FAIL ")
        || (payload.starts_with("test ") && payload.ends_with(" ... FAILED"))
        || (payload.starts_with("thread '") && payload.contains(" panicked at "))
        || payload.starts_with("error[")
        || payload.starts_with("error:")
        || payload.starts_with("assertion ")
}

/// Anchors whose following lines are the failing test's captured output.
/// Compiler `error[` / `error:` lines keep the short context window instead,
/// so a long build log still leaves room for every later anchor.
fn is_test_block_anchor(payload: &str) -> bool {
    payload.starts_with("FAIL ")
        || (payload.starts_with("test ") && payload.ends_with(" ... FAILED"))
        || (payload.starts_with("thread '") && payload.contains(" panicked at "))
        || payload.starts_with("assertion ")
}

fn is_nextest_status_line(payload: &str) -> bool {
    if let Some((status, rest)) = payload.split_once(" [")
        && is_nextest_status_label(status)
        && let Some((inside, _)) = rest.split_once(']')
        && inside.contains('s')
        && inside
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | ' ' | 's'))
    {
        return true;
    }
    // Checked-in fixtures use `PASS name` without nextest's duration bracket.
    matches!(payload.split_whitespace().next(), Some("PASS" | "SKIP"))
}

fn is_nextest_status_label(status: &str) -> bool {
    !status.is_empty()
        && status.len() <= 24
        && status.chars().all(|ch| {
            ch.is_ascii_uppercase() || ch.is_ascii_digit() || matches!(ch, ' ' | '+' | '-')
        })
}

fn is_libtest_status_line(payload: &str) -> bool {
    let Some(rest) = payload.strip_prefix("test ") else {
        return false;
    };
    let Some((_, status)) = rest.rsplit_once(" ... ") else {
        return false;
    };
    matches!(
        status.split_whitespace().next(),
        Some("ok" | "FAILED" | "ignored")
    )
}

fn is_captured_section_header(payload: &str) -> bool {
    let lowered = payload.to_ascii_lowercase();
    if lowered.starts_with("---- ")
        && (lowered.ends_with(" stdout ----") || lowered.ends_with(" stderr ----"))
    {
        return true;
    }
    let core = payload
        .trim_matches(|ch: char| ch.is_whitespace() || ch == '\u{2500}' || ch == '-')
        .to_ascii_lowercase();
    matches!(core.as_str(), "stdout" | "stderr" | "output" | "execfail")
}

/// Nextest prints a rule of box-drawing dashes before the summary.
fn is_section_rule(payload: &str) -> bool {
    let trimmed = payload.trim();
    trimmed.chars().count() >= 8 && trimmed.chars().all(|ch| ch == '\u{2500}')
}

/// Distinct from `[... N command bytes omitted ...]`, which is an ordinary gap
/// between retained regions. The wording avoids signature marker terms
/// (`error`, `failed`, `failure`, `panicked`, `assertion`) so the sweep's
/// normalized signature stays the anchor line.
fn output_block_cut_marker(columns: Option<&str>, bytes: usize) -> String {
    format!(
        "{}[... output block cut; {bytes} bytes not retained ...]\n",
        columns.unwrap_or_default()
    )
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
