//! Nextest failures are useful even when no unique runner command can be selected.
//! Source offsets count original stream bytes, before redaction or display cuts.

use orbit_common::security::redaction::redact_all;
use serde_json::{Value, json};

use super::MAX_CHECKOUT_LOG_SCAN_BYTES;
use super::diagnostic::{
    is_nextest_status_line, is_section_rule, runner_payload, strip_ansi_sequences,
};

const MAX_LINE_BYTES: usize = 16_384;
const MAX_BLOCK_BYTES: usize = 16_384;
const MAX_BLOCKS: usize = 128;
const MAX_TEXT_BYTES: usize = 65_536;

#[derive(Default)]
pub(super) struct NextestRegions {
    offset: usize,
    line: Vec<u8>,
    line_bytes: usize,
    blocks: Vec<Block>,
    open: bool,
    complete: bool,
    initialized: bool,
    anchors: usize,
    command: Option<usize>,
    command_columns: Option<String>,
    command_conflict: bool,
    failed_commands: usize,
    in_command_display: bool,
}

struct Block {
    start: usize,
    end: usize,
    text: String,
    columns: String,
    command: Option<usize>,
    command_complete: bool,
}

impl NextestRegions {
    pub(super) fn push(&mut self, chunk: &[u8]) {
        if !self.initialized {
            self.complete = true;
            self.initialized = true;
        }
        for byte in chunk {
            self.offset = self.offset.saturating_add(1);
            if self.offset > MAX_CHECKOUT_LOG_SCAN_BYTES {
                self.complete = false;
                continue;
            }
            self.line_bytes += 1;
            if self.line.len() < MAX_LINE_BYTES {
                self.line.push(*byte);
            }
            if *byte == b'\n' {
                self.observe_line();
            }
        }
    }

    fn observe_line(&mut self) {
        let mut bytes = std::mem::take(&mut self.line);
        let total = std::mem::take(&mut self.line_bytes);
        self.observe_bytes(&bytes, total);
        bytes.clear();
        self.line = bytes;
    }

    fn observe_bytes(&mut self, bytes: &[u8], total: usize) {
        let start = self.offset - total;
        let line = String::from_utf8_lossy(bytes);
        if let Err(error) = std::str::from_utf8(bytes)
            && (error.error_len().is_some() || total == bytes.len())
        {
            self.complete = false;
        }
        // Outside a command or captured block, a short line cannot be a
        // status, command boundary, or truncation notice. UTF-8 validity and
        // original source offsets were still checked above.
        if !self.open && self.command.is_none() && line.len() < 7 {
            return;
        }
        let (columns, payload) = runner_payload(&line);
        let plain = strip_ansi_sequences(payload);
        let payload = plain.trim();
        let lower = payload.to_ascii_lowercase();
        if !self.in_command_display
            && (payload.starts_with("##[warning]")
                || payload.starts_with("##[error]")
                || payload.starts_with("Log output")
                || payload.starts_with("[..."))
            && lower.contains("log")
            && (lower.contains("truncat") || lower.contains("omitted"))
        {
            self.complete = false;
        }
        let command_start = !self.in_command_display && payload.starts_with("##[group]Run ");
        if self.in_command_display {
            if plain.trim_end() == "##[endgroup]" {
                self.in_command_display = false;
            }
            if self.command.is_some() {
                self.command_conflict |= self.command_columns.as_deref() != columns;
            }
            return;
        }
        if command_start {
            self.in_command_display = true;
        }
        let exit_code = payload
            .strip_prefix("##[error]Process completed with exit code ")
            .and_then(|code| code.strip_suffix('.'))
            .and_then(|code| code.parse::<i32>().ok());
        let command_end = exit_code.is_some();
        let status = is_nextest_status_line(payload);
        let failure = payload.starts_with("FAIL [") && status;
        if status
            || is_section_rule(payload)
            || payload.starts_with("Summary ")
            || command_start
            || command_end
            || self
                .blocks
                .last()
                .is_some_and(|block| block.columns != columns.unwrap_or_default())
        {
            self.open = false;
        }
        if command_start {
            self.command = Some(start);
            self.command_columns = columns.map(ToOwned::to_owned);
            self.command_conflict = false;
        }
        if self.command.is_some() {
            self.command_conflict |= self.command_columns.as_deref() != columns;
        }
        if command_end {
            if exit_code.is_some_and(|code| code != 0) {
                self.failed_commands += 1;
            }
            for block in &mut self.blocks {
                if block.command.is_some() && block.command == self.command {
                    block.command_complete =
                        !self.command_conflict && exit_code.is_some_and(|code| code != 0);
                }
            }
            self.command = None;
        }
        if failure {
            self.anchors += 1;
            if self.blocks.len() < MAX_BLOCKS {
                self.blocks.push(Block {
                    start,
                    end: start,
                    text: String::new(),
                    columns: columns.unwrap_or_default().to_string(),
                    command: self.command,
                    command_complete: false,
                });
                self.open = true;
            }
        }
        if self.open
            && let Some(block) = self.blocks.last_mut()
        {
            block.end = self.offset;
            // Retain whole physical lines only. This also prevents a long
            // credential being split before the redactor sees it.
            if total == bytes.len() && block.text.len() + total <= MAX_BLOCK_BYTES {
                block.text.push_str(&line);
            }
        }
    }

    pub(super) fn source_complete(&self) -> bool {
        (!self.initialized || self.complete) && self.line_bytes == 0
    }

    pub(super) fn finish(self, total_bytes: usize) -> Option<Value> {
        if self.blocks.is_empty() {
            return None;
        }
        let complete = self.source_complete();
        // Fair shares leave room for every anchor when an earlier block is
        // huge. One oversized block must never crowd out a later failure.
        let budget = MAX_TEXT_BYTES / self.blocks.len();
        // Raw job API logs carry no step columns. Preserve the unique-command
        // proof for them; several commands can only be attributed when gh's
        // columns let the consumer bind every retained block to one step.
        let command_complete = self.blocks.iter().all(|block| block.command_complete)
            && (self.failed_commands == 1
                || self.blocks.iter().all(|block| !block.columns.is_empty()));
        let mut text = String::new();
        let mut ranges = Vec::new();
        let mut retained = 0;
        let mut selection_complete = complete && self.anchors == self.blocks.len();
        for block in self.blocks {
            let mut block_text = redact_all(&block.text);
            let mut columns = redact_all(&block.columns);
            if columns.len() + 100 > budget {
                columns.clear();
                selection_complete = false;
            }
            let limit = budget.saturating_sub(columns.len() + 100);
            let mut kept = block.text.len();
            if block_text.len() > limit {
                let end = block_text
                    .char_indices()
                    .take_while(|(index, _)| *index < limit)
                    .filter(|(_, ch)| *ch == '\n')
                    .map(|(index, _)| index + 1)
                    .last()
                    .unwrap_or(0);
                block_text.truncate(end);
                // Redaction can change byte lengths; source accounting uses
                // the matching number of retained physical lines.
                kept = block
                    .text
                    .split_inclusive('\n')
                    .take(block_text.lines().count())
                    .map(str::len)
                    .sum();
            }
            selection_complete &= kept > 0;
            let omitted = block.end - block.start - kept;
            if omitted > 0 {
                block_text.push_str(&format!(
                    "{}[... output block cut; {omitted} bytes not retained ...]\n",
                    columns
                ));
            }
            retained += kept;
            text.push_str(&block_text);
            ranges.push(json!({"start_byte": block.start, "end_byte": block.end,
                "retained_source_bytes": kept, "omitted_bytes": omitted}));
        }
        Some(json!({
            "kind": "runner_failure_regions", "complete": false,
            "command_complete": command_complete, "selection_complete": selection_complete,
            "text": text, "returned_bytes": text.len(), "command_bytes": total_bytes,
            "retained_source_bytes": retained, "omitted_bytes": total_bytes - retained,
            "assertion_payload_omitted_bytes": 0, "failure_anchor_count": self.anchors,
            "byte_ranges": ranges,
        }))
    }
}
