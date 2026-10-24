use orbit_common::security::redaction::redact_all;

use super::super::diagnostic;
use super::checkout::{CheckoutEvidenceCollector, runner_log_payload, split_log_line};
use super::model::{
    LogSelection, MAX_CHECKOUT_LOG_SCAN_BYTES, STREAM_BATCH_BYTES, StreamedLog,
    WINDOW_TOKEN_CONTEXT_BYTES,
};
use super::selection::{
    WindowBoundaryTokens, cut_continues_token, excerpt_without_orphaned_tail_token,
    excerpt_without_partial_prefix, excerpt_without_partial_suffix,
};

/// Incrementally retain the head/tail excerpt and checkout evidence from a
/// potentially large log. `scan_limit` makes incomplete identity explicit
/// instead of retaining an unbounded source stream.
///
/// A truncated excerpt drops a whitespace-delimited token split by either
/// retained boundary before redaction. A raw cut would otherwise keep a
/// credential prefix or suffix that `redact_all` cannot match. The head cut
/// keeps one adjacent byte. The tail cut keeps a bounded lookbehind of
/// display bytes (at most 16 KiB) so a value whose introducer fell in the
/// omitted middle is omitted with the cut token. Only the two windows, that
/// lookbehind, a bounded 4 KiB input batch, and bounded original line-prefix
/// context for selection boundaries are retained. `finish` processes the last
/// partial batch before rendering.
pub struct StreamedLogCollector {
    max_bytes: usize,
    /// How much of the budget the head window may keep; the rest is the tail.
    head_bytes: usize,
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    pending: Vec<u8>,
    total_bytes: usize,
    /// First byte after `head` once that window is full. Absent when the
    /// head was never cut.
    head_following: Option<u8>,
    /// Display bytes immediately before the retained tail, capped at
    /// [`WINDOW_TOKEN_CONTEXT_BYTES`]. This is the excerpt-cut lookbehind:
    /// source coordinates and `window_tokens` stay on the selection boundary.
    tail_dropped: Vec<u8>,
    evidence: CheckoutEvidenceCollector,
    diagnostic: diagnostic::DiagnosticCollector,
    nextest: super::super::nextest_regions::NextestRegions,
    selection: Option<LogSelection>,
    window_tokens: WindowBoundaryTokens,
    line_tokens: WindowBoundaryTokens,
    /// Coordinates in the payload-only stream used by `line_tokens`.
    /// Display selection and accounting continue to use original source bytes.
    payload_line_offset: usize,
    selected_segment_start: Option<usize>,
    scoped_line: Vec<u8>,
    candidate_byte_offset: usize,
    candidate_line_index: usize,
    selected_bytes: usize,
    selected_lines: usize,
    selection_found: bool,
    plain_step_active: bool,
    plain_group_depth: usize,
    plain_log_seen: bool,
}

impl StreamedLogCollector {
    /// An even head/tail split, for a source whose every line is relevant —
    /// `gh run view --log-failed` emits only failed steps, so its head names
    /// the failing command and its tail carries the assertion.
    pub fn new(max_bytes: usize, max_evidence_lines: usize) -> Self {
        Self::with_head_bytes(max_bytes, max_bytes / 2, max_evidence_lines)
    }

    /// An even split over a scoped selection (see [`LogSelection`]).
    pub fn scoped(max_bytes: usize, max_evidence_lines: usize, selection: LogSelection) -> Self {
        let mut collector = Self::with_head_bytes(max_bytes, max_bytes / 2, max_evidence_lines);
        collector.selection = Some(selection);
        collector.selection_found = false;
        collector
    }

    /// A tail-weighted split over a scoped selection.
    pub fn tail_weighted_scoped(
        max_bytes: usize,
        max_evidence_lines: usize,
        selection: LogSelection,
    ) -> Self {
        let mut collector = Self::with_head_bytes(max_bytes, max_bytes / 4, max_evidence_lines);
        collector.selection = Some(selection);
        collector.selection_found = false;
        collector
    }

    /// A tail-weighted split, for a whole-job log.
    ///
    /// Such a log opens with runner provisioning and setup and only reaches
    /// the failing step near its end, so an even split spends half the budget
    /// on boilerplate and can cut the diagnostic headline off. A quarter is
    /// still enough head to name the job and its first command.
    pub fn tail_weighted(max_bytes: usize, max_evidence_lines: usize) -> Self {
        Self::with_head_bytes(max_bytes, max_bytes / 4, max_evidence_lines)
    }

    fn with_head_bytes(max_bytes: usize, head_bytes: usize, max_evidence_lines: usize) -> Self {
        Self {
            max_bytes,
            head_bytes,
            head: Vec::with_capacity(head_bytes),
            tail: std::collections::VecDeque::with_capacity(max_bytes.saturating_sub(head_bytes)),
            pending: Vec::with_capacity(STREAM_BATCH_BYTES),
            total_bytes: 0,
            head_following: None,
            tail_dropped: Vec::new(),
            diagnostic: diagnostic::DiagnosticCollector::default(),
            nextest: super::super::nextest_regions::NextestRegions::default(),
            evidence: CheckoutEvidenceCollector::new(
                max_evidence_lines,
                MAX_CHECKOUT_LOG_SCAN_BYTES,
            ),
            selection: None,
            window_tokens: WindowBoundaryTokens::default(),
            line_tokens: WindowBoundaryTokens::default(),
            payload_line_offset: 0,
            selected_segment_start: None,
            scoped_line: Vec::new(),
            candidate_byte_offset: 0,
            candidate_line_index: 0,
            selected_bytes: 0,
            selected_lines: 0,
            selection_found: true,
            plain_step_active: false,
            plain_group_depth: 0,
            plain_log_seen: false,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.total_bytes = self.total_bytes.saturating_add(chunk.len());
        let mut remaining = chunk;
        while !remaining.is_empty() {
            let take = (STREAM_BATCH_BYTES - self.pending.len()).min(remaining.len());
            self.pending.extend_from_slice(&remaining[..take]);
            remaining = &remaining[take..];
            if self.pending.len() == STREAM_BATCH_BYTES {
                self.flush_pending(false);
            }
        }
    }

    fn flush_pending(&mut self, final_batch: bool) {
        let mut pending = std::mem::take(&mut self.pending);
        let take = if final_batch {
            pending.len()
        } else {
            // Checkout evidence decodes each batch. Leave an unfinished UTF-8
            // character for the next read instead of replacing its pieces.
            match std::str::from_utf8(&pending) {
                Err(error) if error.error_len().is_none() => error.valid_up_to(),
                _ => pending.len(),
            }
        };
        self.collect_batch(&pending[..take]);
        pending.drain(..take);
        self.pending = pending;
    }

    fn collect_batch(&mut self, chunk: &[u8]) {
        self.evidence.push(chunk);
        self.diagnostic.push(chunk);
        self.nextest.push(chunk);

        if let Some(selection) = self.selection.clone() {
            if selection.needs_line_mode() {
                self.scoped_line.extend_from_slice(chunk);
                while let Some(end) = self.scoped_line.iter().position(|byte| *byte == b'\n') {
                    let line: Vec<u8> = self.scoped_line.drain(..=end).collect();
                    self.process_scoped_line(&line, &selection);
                }
            } else {
                self.process_scoped_bytes(chunk, &selection);
            }
            return;
        }
        self.feed_display(chunk);
    }

    fn process_scoped_bytes(&mut self, chunk: &[u8], selection: &LogSelection) {
        let start = self.candidate_byte_offset;
        self.candidate_byte_offset = self.candidate_byte_offset.saturating_add(chunk.len());
        let Some((window_start, window_end)) = selection.byte_window else {
            self.feed_display(chunk);
            self.selected_bytes = self.selected_bytes.saturating_add(chunk.len());
            return;
        };
        self.window_tokens
            .push(chunk, start, window_start, window_end);
        let end = start.saturating_add(chunk.len());
        let from = window_start.max(start);
        let to = window_end.unwrap_or(usize::MAX).min(end);
        if from < to {
            let slice = &chunk[from - start..to - start];
            self.selection_found = true;
            self.selected_bytes = self.selected_bytes.saturating_add(slice.len());
            self.feed_display(slice);
        }
    }

    fn process_scoped_line(&mut self, line: &[u8], selection: &LogSelection) {
        let in_step = selection
            .step
            .as_ref()
            .is_none_or(|step| self.line_belongs_to_step(line, step));
        let index = self.candidate_line_index;
        if in_step {
            self.candidate_line_index = self.candidate_line_index.saturating_add(1);
        }
        let in_window = selection
            .line_window
            .is_none_or(|(start, end)| index >= start && index < end.unwrap_or(usize::MAX));
        let selected = in_step && in_window;
        // Observe every original line before discarding it. A selected segment
        // can start after a credential introducer, including across a step gap.
        let payload = runner_log_payload(line);
        let payload_start = line.len() - payload.len();
        let start = self.payload_line_offset;
        self.payload_line_offset = start.saturating_add(payload.len());
        let from = if selected {
            *self.selected_segment_start.get_or_insert(start)
        } else {
            self.selected_segment_start = None;
            usize::MAX
        };
        self.line_tokens.push(payload, start, from, None);
        // A final line need not end in whitespace.
        self.line_tokens
            .finish_token(self.payload_line_offset, from, None);
        if !selected {
            // A byte window combined with a line window counts candidate
            // bytes only for lines inside the line window.
            if in_step && selection.byte_window.is_some() {
                self.candidate_byte_offset = self.candidate_byte_offset.saturating_add(line.len());
            }
            return;
        }
        // Mask lost-context tokens in place before the head/tail cut. Keeping
        // byte lengths and whitespace preserves source coordinates, excerpt
        // budgets and ordinary text even for non-contiguous step selections.
        let mask_until = self
            .line_tokens
            .omit_prefix_until
            .saturating_sub(start)
            .min(payload.len());
        let mut display = std::borrow::Cow::Borrowed(line);
        if mask_until > 0 {
            for byte in &mut display.to_mut()[payload_start..payload_start + mask_until] {
                if !byte.is_ascii_whitespace() {
                    *byte = b'*';
                }
            }
        }
        let line = display.as_ref();
        if selection.byte_window.is_some() {
            let before = self.selected_bytes;
            self.process_scoped_bytes(line, selection);
            if self.selected_bytes > before {
                self.selected_lines = self.selected_lines.saturating_add(1);
            }
            return;
        }
        self.selection_found = true;
        self.selected_bytes = self.selected_bytes.saturating_add(line.len());
        self.selected_lines = self.selected_lines.saturating_add(1);
        self.feed_display(line);
    }

    fn line_belongs_to_step(&mut self, line: &[u8], step: &str) -> bool {
        let text = String::from_utf8_lossy(line);
        let text = text.trim_end_matches(['\n', '\r']);
        // Column lines are `job<TAB>step<TAB>…`; a raw line starts with its
        // timestamp, so tabs inside its payload are runtime data, not columns.
        let starts_with_timestamp = text
            .trim_start_matches('\u{feff}')
            .split_once([' ', '\t'])
            .is_some_and(|(first, _)| first.contains('T') && first.ends_with('Z'));
        if text.contains('\t') && !starts_with_timestamp {
            let (column, _) = split_log_line(text);
            return column.trim() == step;
        }
        // A plain job-API log has no step column. A step starts at a
        // top-level `##[group]Run <command>` display group, but that group
        // closes (`##[endgroup]`) before the command runs: the step's
        // runtime output and error annotations follow it. The step therefore
        // runs until the next top-level `Run` group or the end of the log.
        // Groups nested in the runtime output (depth > 0, or any non-`Run`
        // title at the top level) belong to the step and never reset it.
        self.plain_log_seen = true;
        let (_, payload) = split_log_line(text);
        let payload = payload.trim();
        if let Some(title) = payload.strip_prefix("##[group]") {
            let title = title.trim();
            let is_run_group = title == "Run" || title.starts_with("Run ");
            let name = title.strip_prefix("Run ").unwrap_or(title);
            if self.plain_step_active {
                if self.plain_group_depth == 0 && is_run_group {
                    self.plain_step_active = false;
                    self.plain_group_depth = 1;
                    return false;
                }
                self.plain_group_depth = self.plain_group_depth.saturating_add(1);
                return true;
            }
            if self.plain_group_depth == 0 && (name == step || title == step) {
                self.plain_step_active = true;
                self.plain_group_depth = 1;
                return true;
            }
            self.plain_group_depth = self.plain_group_depth.saturating_add(1);
            return false;
        }
        if payload.starts_with("##[endgroup]") {
            let was_active = self.plain_step_active;
            self.plain_group_depth = self.plain_group_depth.saturating_sub(1);
            return was_active;
        }
        self.plain_step_active
    }

    fn feed_display(&mut self, chunk: &[u8]) {
        let head_limit = self.head_bytes;
        let head_take = head_limit.saturating_sub(self.head.len()).min(chunk.len());
        self.head.extend_from_slice(&chunk[..head_take]);
        if self.head_following.is_none()
            && head_limit > 0
            && self.head.len() == head_limit
            && head_take < chunk.len()
        {
            self.head_following = Some(chunk[head_take]);
        }
        let tail_limit = self.max_bytes.saturating_sub(head_limit);
        let tail_chunk = &chunk[head_take..];
        if tail_limit == 0 {
            return;
        }
        if tail_chunk.len() >= tail_limit {
            let discarded_prefix = tail_chunk.len() - tail_limit;
            // The previous tail sits immediately before this chunk. Keep it
            // in the lookbehind, then the chunk bytes that did not fit.
            let old_tail: Vec<u8> = self.tail.drain(..).collect();
            self.remember_dropped_before_tail(&old_tail);
            self.remember_dropped_before_tail(&tail_chunk[..discarded_prefix]);
            self.tail.extend(&tail_chunk[discarded_prefix..]);
        } else {
            let drain_end = (self.tail.len() + tail_chunk.len()).saturating_sub(tail_limit);
            if drain_end > 0 {
                // A front drain advances the ring; it does not move the
                // retained tail on every single-byte transport read.
                let discarded: Vec<u8> = self.tail.drain(..drain_end).collect();
                self.remember_dropped_before_tail(&discarded);
            }
            self.tail.extend(tail_chunk);
        }
    }

    /// Remember display bytes that just fell out of the tail ring.
    ///
    /// Only the suffix that fits in [`WINDOW_TOKEN_CONTEXT_BYTES`] is kept.
    /// An introducer older than that bound is outside the excerpt-cut
    /// context; selection-boundary classification still applies its own
    /// overlong rule.
    fn remember_dropped_before_tail(&mut self, dropped: &[u8]) {
        if dropped.is_empty() {
            return;
        }
        let keep = WINDOW_TOKEN_CONTEXT_BYTES;
        if dropped.len() >= keep {
            self.tail_dropped.clear();
            self.tail_dropped
                .extend_from_slice(&dropped[dropped.len() - keep..]);
            return;
        }
        let overflow = (self.tail_dropped.len() + dropped.len()).saturating_sub(keep);
        if overflow > 0 {
            self.tail_dropped.drain(..overflow);
        }
        self.tail_dropped.extend_from_slice(dropped);
    }

    pub fn finish(mut self) -> StreamedLog {
        // Include the short stream or final partial batch in every collector
        // and both retained windows before rendering or checking completeness.
        self.flush_pending(true);
        if let Some(selection) = self.selection.clone()
            && selection.needs_line_mode()
            && !self.scoped_line.is_empty()
        {
            let line = std::mem::take(&mut self.scoped_line);
            self.process_scoped_line(&line, &selection);
        }
        if let Some((start, end)) = self.selection.as_ref().and_then(|s| s.byte_window) {
            self.window_tokens
                .finish_token(self.candidate_byte_offset, start, end);
        }
        // Outer selection cuts need the original tokens, even when the
        // selected bytes fit the display budget. Keep these independent of
        // the inner head/tail excerpt cuts.
        let window_start = self
            .selection
            .as_ref()
            .and_then(|s| s.byte_window)
            .map_or(0, |w| w.0);
        let evidence = self.evidence.finish();
        let display_total = if self.selection.is_some() {
            self.selected_bytes
        } else {
            self.total_bytes
        };
        let truncated = display_total > self.max_bytes;
        let mut text = if truncated {
            let omitted = display_total.saturating_sub(self.head.len() + self.tail.len());
            // Redact only after the cut token is gone. Joining the windows
            // would keep the omitted middle, which this collector must not
            // buffer. The head cut keeps one adjacent byte. The tail cut
            // keeps a bounded display lookbehind so a value split from its
            // introducer is omitted before redaction.
            let head = self.window_tokens.safe_slice(&self.head, 0, window_start);
            let head =
                excerpt_without_partial_suffix(head, cut_continues_token(self.head_following));
            let tail_offset = display_total.saturating_sub(self.tail.len());
            let raw_tail = self.tail.make_contiguous();
            let tail = self
                .window_tokens
                .safe_slice(raw_tail, tail_offset, window_start);
            // `safe_slice` drops selection-boundary tokens in source
            // coordinates. When that trim removes the start of the tail, the
            // display lookbehind is no longer adjacent to the retained bytes;
            // keep the existing one-byte partial-token drop for that case.
            let tail_continues = cut_continues_token(self.tail_dropped.last().copied());
            let prefix_kept =
                tail.len() == raw_tail.len() || std::ptr::eq(tail.as_ptr(), raw_tail.as_ptr());
            let tail = if prefix_kept {
                excerpt_without_orphaned_tail_token(tail, &self.tail_dropped)
            } else {
                excerpt_without_partial_prefix(tail, tail_continues)
            };
            format!(
                "{}\n[... {omitted} bytes omitted; raise the byte budget for more ...]\n{}",
                redact_all(&head),
                redact_all(&tail),
            )
        } else {
            // When the source is short, all bytes are in `tail` except the
            // initial head window, so join the two retained halves.
            self.head.extend(self.tail);
            let text = self.window_tokens.safe_slice(&self.head, 0, window_start);
            redact_all(&String::from_utf8_lossy(text))
        };
        let nextest_complete = self.nextest.source_complete();
        let nextest = self.nextest.finish(self.total_bytes);
        let source_complete = if nextest.is_some() {
            nextest_complete
        } else {
            self.diagnostic.source_complete()
        };
        let (diagnostic, mut failure_regions) = self.diagnostic.finish();
        if let Some(nextest) = nextest {
            if truncated
                && self.selection.is_none()
                && let Some(blocks) = nextest["text"].as_str()
            {
                text.push_str("\n[... retained nextest failure blocks ...]\n");
                text.push_str(blocks);
            }
            failure_regions = Some(nextest);
        }
        StreamedLog {
            returned_bytes: text.len(),
            text,
            truncated,
            total_bytes: self.total_bytes,
            checkout_evidence: evidence,
            source_complete,
            diagnostic,
            failure_regions,
            selected_bytes: if self.selection.is_some() {
                self.selected_bytes
            } else {
                self.total_bytes
            },
            selected_lines: self.selected_lines,
            selection_found: self.selection_found,
            selection_unsupported: self.selection.as_ref().is_some_and(|s| s.step.is_some())
                && !self.selection_found
                && self.plain_log_seen,
            selection: self.selection.clone(),
        }
    }
}
