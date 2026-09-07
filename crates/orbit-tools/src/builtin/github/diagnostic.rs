//! Complete runner command units, retained separately from head/tail display.
//!
//! A unit starts at the runner's `Run` group and ends at its nonzero process
//! completion. Only a unique such unit is returned. The caller must still
//! verify the supplying job has exactly one failed step. We do not infer
//! completeness from an error headline or from EOF alone.

use orbit_common::security::redaction::redact_all;

use super::MAX_CHECKOUT_LOG_SCAN_BYTES;

const MAX_UNIT_BYTES: usize = 262_144;
const MAX_LINE_BYTES: usize = 16_384;

#[derive(Default)]
pub(super) struct DiagnosticCollector {
    scanned: usize,
    line: Vec<u8>,
    invalid: bool,
    candidate: Option<String>,
    selected: Option<String>,
    failures: usize,
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
            self.line.push(*byte);
            if self.line.len() > MAX_LINE_BYTES {
                self.invalid = true;
                return;
            }
            if *byte == b'\n' {
                let bytes = std::mem::take(&mut self.line);
                let Ok(line) = std::str::from_utf8(&bytes) else {
                    self.invalid = true;
                    return;
                };
                self.observe(line);
            }
        }
    }

    fn observe(&mut self, line: &str) {
        // Keep the original columns, timestamps and ANSI bytes as evidence;
        // only inspect the runner payload to recognize structural boundaries.
        let payload = line.rsplit('\t').next().unwrap_or(line).trim();
        let payload = payload
            .split_once(' ')
            .filter(|(prefix, _)| prefix.ends_with('Z') && prefix.contains('T'))
            .map_or(payload, |(_, rest)| rest);
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
        if payload.starts_with("##[group]Run ") {
            self.candidate = Some(String::new());
        }
        if let Some(candidate) = &mut self.candidate {
            if candidate.len() + line.len() > MAX_UNIT_BYTES {
                self.candidate = None;
            } else {
                candidate.push_str(line);
            }
        }
        if payload
            .strip_prefix("##[error]Process completed with exit code ")
            .and_then(|code| code.strip_suffix('.'))
            .and_then(|code| code.parse::<i32>().ok())
            .is_some_and(|code| code != 0)
        {
            self.failures += 1;
            self.selected = self.candidate.take();
        }
    }

    pub(super) fn source_complete(&self) -> bool {
        !self.invalid
    }

    pub(super) fn finish(self) -> Option<String> {
        if self.invalid || !self.line.is_empty() || self.failures != 1 {
            return None;
        }
        self.selected.map(|unit| redact_all(&unit))
    }
}
