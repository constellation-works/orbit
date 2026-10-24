use super::*;

/// Prose a runner emits around a checkout. Matched case-insensitively.
const CHECKOUT_MARKERS: &[&str] = &[
    "head is now at",
    "-> fetch_head",
    "git checkout --progress",
    "checking out the ref",
    "checkout ref",
];

const MIN_SHA_LEN: usize = 7;
const MAX_SHA_LEN: usize = 40;
/// The most source bytes checkout extraction will inspect from one log. The
/// rest is still drained so `gh` can exit, but identity is marked incomplete.
pub const MAX_CHECKOUT_LOG_SCAN_BYTES: usize = 8 * 1024 * 1024;
const MAX_CHECKOUT_EVIDENCE_LINE_BYTES: usize = 16 * 1024;
/// Bound and amortize work across small transport reads.
const STREAM_BATCH_BYTES: usize = 4096;
/// Original line-prefix/token context for selection cuts. Context beyond
/// this bound is conservatively omitted rather than retaining a credential.
const WINDOW_TOKEN_CONTEXT_BYTES: usize = 16 * 1024;

/// The commit a runner actually checked out, read out of the runner's own log.
///
/// A workflow event's reported head SHA is metadata; this is evidence. They
/// disagree whenever a merge-queue or pull-request merge commit is what got
/// tested, so the two travel as separate fields and are never merged into one
/// `sha`.
pub struct CheckoutEvidence {
    /// The distinct commits the log named as checked out, with an
    /// abbreviation collapsed into the full SHA it prefixes. More than one
    /// entry means the log genuinely disagreed with itself.
    pub commits: Vec<String>,
    pub lines: Vec<String>,
    /// False only when identity itself may have been missed: the source byte
    /// cap (`source_truncated`) was hit, or a dropped overlong line could
    /// plausibly have carried checkout identity. A reduced *display* — the
    /// evidence line/commit count cap, or an overlong line unrelated to
    /// checkout — does not clear this; see `display_truncated`.
    pub complete: bool,
    pub scanned_bytes: usize,
    pub source_truncated: bool,
    /// True when what is reported was capped for display reasons that do not
    /// bear on checkout identity: the evidence line or commit count limit was
    /// reached, or an overlong line unrelated to checkout was dropped.
    pub display_truncated: bool,
}

/// A scoped selection over a log stream: a named step and/or a byte or line
/// window. Byte offsets are 0-based and end-exclusive over the candidate
/// bytes; line numbers are 1-based and end-exclusive over candidate lines.
/// When a step is present the candidate is that step's lines, and a window
/// then applies within the step section; otherwise the candidate is the
/// whole log. Windows are still bounded by the caller's `max_bytes` excerpt
/// cap, with the usual omission marker when the selection exceeds it.
/// Coordinates and counts refer to source bytes, before redaction. A cut
/// credential token (or overlong context whose safety cannot be established)
/// is omitted from the display; ordinary byte windows remain exact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogSelection {
    pub step: Option<String>,
    pub byte_window: Option<(usize, Option<usize>)>,
    pub line_window: Option<(usize, Option<usize>)>,
}

impl LogSelection {
    pub fn step(step: String) -> Self {
        Self {
            step: Some(step),
            byte_window: None,
            line_window: None,
        }
    }

    fn needs_line_mode(&self) -> bool {
        self.step.is_some() || self.line_window.is_some()
    }

    pub fn describe(&self, selected_bytes: usize, selected_lines: usize) -> Value {
        let (byte_start, byte_end) = match self.byte_window {
            Some((start, end)) => (Some(start), end),
            None => (None, None),
        };
        let (line_start, line_end) = match self.line_window {
            // Stored 0-based; report the caller's 1-based numbers.
            Some((start, end)) => (Some(start + 1), end.map(|end| end + 1)),
            None => (None, None),
        };
        serde_json::json!({
            "step": self.step,
            "byte_start": byte_start,
            "byte_end": byte_end,
            "line_start": line_start,
            "line_end": line_end,
            "selected_bytes": selected_bytes,
            "selected_lines": selected_lines,
        })
    }
}

/// A bounded excerpt and checkout evidence collected while a log is drained.
/// No unbounded copy of the source log is retained.
pub struct StreamedLog {
    pub text: String,
    pub truncated: bool,
    pub total_bytes: usize,
    pub returned_bytes: usize,
    pub checkout_evidence: CheckoutEvidence,
    /// Complete runner command evidence, independent of display truncation.
    pub diagnostic: Option<String>,
    /// Selected command regions or independently collected nextest blocks,
    /// including original source byte ranges for recognized nextest failures.
    pub failure_regions: Option<Value>,
    /// False when a source limit, invalid line, or explicit truncation notice
    /// prevents complete diagnostic collection; independent of display size.
    pub source_complete: bool,
    /// The scoped selection applied to `text`, when one was requested.
    pub selection: Option<LogSelection>,
    /// Bytes in the selected section/window before the excerpt cap.
    pub selected_bytes: usize,
    /// Lines in the selected section/window (line/step selections).
    pub selected_lines: usize,
    /// Whether the requested step/window matched anything in this source.
    /// Always true for an unscoped read.
    pub selection_found: bool,
    /// A step was requested, none matched, and the source was a raw
    /// (uncolumned) log: its only step titles are `Run <command>` display
    /// groups, so an unmatched name may be a workflow step name that maps
    /// to a command rather than an absent step.
    pub selection_unsupported: bool,
}

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
    nextest: super::nextest_regions::NextestRegions,
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
            nextest: super::nextest_regions::NextestRegions::default(),
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

/// Inspect tokens with their original line prefix, including credential
/// introducers that a byte window may omit or cut. Only display bounds change;
/// source coordinates and selected counts continue to describe the raw stream.
#[derive(Default)]
struct WindowBoundaryTokens {
    context: Vec<u8>,
    context_start: usize,
    token_start: usize,
    overlong: bool,
    omit_prefix_until: usize,
    omit_suffix_from: Option<usize>,
}

impl WindowBoundaryTokens {
    fn push(&mut self, chunk: &[u8], start: usize, from: usize, to: Option<usize>) {
        let mut offset = start;
        for part in chunk.split_inclusive(u8::is_ascii_whitespace) {
            // The token crossing the end still needs its complete source.
            if to.is_some_and(|end| self.token_start >= end) {
                break;
            }
            let whitespace = part.last().copied().filter(u8::is_ascii_whitespace);
            let token_part = &part[..part.len() - usize::from(whitespace.is_some())];
            self.extend_context(token_part);
            offset = offset.saturating_add(token_part.len());
            if let Some(whitespace) = whitespace {
                self.finish_token(offset, from, to);
                offset = offset.saturating_add(1);
                self.token_start = offset;
                // Bearer/header/assignment whitespace can cross a newline.
                // Keep a pending introducer, still under the same byte cap.
                let line_break = matches!(whitespace, b'\n' | b'\r');
                let pending_introducer = line_break
                    && self
                        .context
                        .split(u8::is_ascii_whitespace)
                        .rfind(|part| !part.is_empty())
                        .is_some_and(|part| {
                            part.eq_ignore_ascii_case(b"bearer")
                                || matches!(part.last(), Some(b':' | b'='))
                        });
                if line_break && !pending_introducer {
                    self.context.clear();
                    self.context_start = offset;
                    self.overlong = false;
                } else {
                    self.extend_context(&[whitespace]);
                }
            }
        }
    }

    fn extend_context(&mut self, bytes: &[u8]) {
        let take = WINDOW_TOKEN_CONTEXT_BYTES
            .saturating_sub(self.context.len())
            .min(bytes.len());
        self.context.extend_from_slice(&bytes[..take]);
        self.overlong |= take < bytes.len();
    }

    fn finish_token(&mut self, end: usize, from: usize, to: Option<usize>) {
        if self.token_start == end || end <= from || to.is_some_and(|to| self.token_start >= to) {
            return;
        }
        let cuts_end = to.is_some_and(|to| self.token_start < to && to < end);
        let lost_prefix = self.context_start < from;
        if !cuts_end && !lost_prefix {
            return;
        }
        let token_index = self
            .token_start
            .saturating_sub(self.context_start)
            .min(self.context.len());
        let prefix = String::from_utf8_lossy(&self.context[..token_index]);
        let token = String::from_utf8_lossy(&self.context[token_index..]);
        let sensitive = self.overlong || Self::context_redacts_token(&prefix, &token);
        if !sensitive {
            return;
        }
        if cuts_end {
            self.omit_suffix_from = Some(
                self.omit_suffix_from
                    .map_or(self.token_start, |old| old.min(self.token_start)),
            );
        }
        if lost_prefix {
            // A cut in an introducer can leave the following value whole yet
            // unrecognizable. Check whether the retained prefix still gives
            // the redactor enough context before preserving that value.
            let visible_index = from.saturating_sub(self.context_start).min(token_index);
            let visible_prefix = String::from_utf8_lossy(&self.context[visible_index..token_index]);
            if self.overlong
                || self.token_start < from
                || !Self::context_redacts_token(&visible_prefix, &token)
            {
                self.omit_prefix_until = end;
            }
        }
    }

    fn context_redacts_token(prefix: &str, token: &str) -> bool {
        // Assignments are credential introducers even when the generic text
        // redactor only recognizes JSON/header spellings. The separator may
        // precede this token (key = value) or belong to it (key=value/key =value).
        let assignment_key = if let Some(key) = prefix.trim_end().strip_suffix('=') {
            key.split_whitespace().last()
        } else {
            token.split_once('=').and_then(|(key, _)| {
                if key.is_empty() {
                    prefix.split_whitespace().last()
                } else {
                    Some(key)
                }
            })
        };
        let api_key_assignment = assignment_key.is_some_and(|key| {
            let key = key.trim_matches(['\'', '"', '[', ']', '{', '}', '(', ')']);
            // Normalize separators and case for one suffix check, while
            // retaining source positions to distinguish `capiKey` from a
            // prefixed API-key segment.
            let normalized: Vec<_> = key
                .char_indices()
                .filter_map(|(index, character)| {
                    character
                        .is_ascii_alphanumeric()
                        .then_some((character.to_ascii_lowercase(), index))
                })
                .collect();
            if normalized.len() < 6
                || !normalized
                    .iter()
                    .map(|(character, _)| *character)
                    .collect::<String>()
                    .ends_with("apikey")
            {
                return false;
            }

            let api_start_in_normalized = normalized.len() - 6;
            if api_start_in_normalized == 0 {
                return true;
            }
            let api_start = normalized[api_start_in_normalized].1;
            let prefix_end = normalized[api_start_in_normalized - 1].1;
            let previous = key.as_bytes()[prefix_end];
            let api_a = key.as_bytes()[api_start];
            let api_p = key.as_bytes()[normalized[api_start_in_normalized + 1].1];
            let separated = key[prefix_end + 1..api_start]
                .bytes()
                .any(|byte| !byte.is_ascii_alphanumeric());
            let camel_case_boundary = api_a.is_ascii_uppercase()
                && ((previous.is_ascii_lowercase() || previous.is_ascii_digit())
                    || (previous.is_ascii_uppercase() && api_p.is_ascii_lowercase()));
            let uppercase_api = normalized[api_start_in_normalized..api_start_in_normalized + 3]
                .iter()
                .all(|(_, index)| key.as_bytes()[*index].is_ascii_uppercase());

            // Accept separator-delimited keys, case transitions (including
            // an acronym followed by `Api`), and an all-caps `API` segment.
            separated || camel_case_boundary || uppercase_api
        });
        api_key_assignment
            || redact_all(&format!("{prefix}{token}")) != format!("{}{token}", redact_all(prefix))
    }

    fn safe_slice<'a>(&self, bytes: &'a [u8], offset: usize, window_start: usize) -> &'a [u8] {
        let start = self.omit_prefix_until.saturating_sub(window_start);
        let end = self
            .omit_suffix_from
            .map_or(usize::MAX, |end| end.saturating_sub(window_start));
        let from = start.saturating_sub(offset).min(bytes.len());
        let to = end.saturating_sub(offset).min(bytes.len());
        &bytes[from.min(to)..to]
    }
}

/// Whether `adjacent` belongs to the same whitespace-delimited token as the
/// excerpt byte on the other side of the cut.
fn cut_continues_token(adjacent: Option<u8>) -> bool {
    adjacent.is_some_and(|byte| byte.is_ascii() && !byte.is_ascii_whitespace())
}

/// Drop a token split by the end of the retained head.
///
/// Pattern redaction matches a whole credential (`ghp_` plus 36 token
/// characters, and the other fixed shapes). The head cut keeps a prefix of
/// that credential and the tail cut keeps the suffix without its prefix, so
/// neither window matches. Removing the partial token leaves every complete
/// token in the window for `redact_all`.
fn excerpt_without_partial_suffix(bytes: &[u8], cut_token: bool) -> String {
    let text = String::from_utf8_lossy(bytes);
    if !cut_token {
        return text.into_owned();
    }
    match text.rfind(char::is_whitespace) {
        Some(index) => {
            let len = text[index..].chars().next().map_or(0, char::len_utf8);
            text[..index + len].to_string()
        }
        None => String::new(),
    }
}

/// Drop a token split by the start of the retained tail. See
/// [`excerpt_without_partial_suffix`].
fn excerpt_without_partial_prefix(bytes: &[u8], cut_token: bool) -> String {
    let text = String::from_utf8_lossy(bytes);
    if !cut_token {
        return text.into_owned();
    }
    match text.find(char::is_whitespace) {
        Some(index) => text[index..].to_string(),
        None => String::new(),
    }
}

/// Drop a partial tail token, then a following whole token whose introducer
/// the excerpt cut left in the display lookbehind.
///
/// `redact_all` only matches a value while its introducer is still in the
/// same window. A value that is already a whole token at the tail start
/// (the preceding byte is whitespace, or the cut token was only the
/// introducer's suffix) would otherwise be returned in clear. Tokens that
/// the redactor or the assignment check recognize on their own stay, so a
/// complete credential is masked in place instead of disappearing. A newline
/// reset before that token drops the lookbehind, so ordinary text after a
/// long non-credential span stays readable.
fn excerpt_without_orphaned_tail_token(bytes: &[u8], lookbehind: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let continues = cut_continues_token(lookbehind.last().copied());
    if continues && !text.contains(char::is_whitespace) {
        return String::new();
    }
    let split_at = if continues {
        text.find(char::is_whitespace).unwrap_or(0)
    } else {
        0
    };
    let partial = &text[..split_at];
    let rest = &text[split_at..];
    let Some(ws_len) = rest.find(|character: char| !character.is_whitespace()) else {
        return rest.to_string();
    };
    let body = &rest[ws_len..];
    let token_len = body.find(char::is_whitespace).unwrap_or(body.len());
    let token = &body[..token_len];
    let prefix = tail_token_prefix(lookbehind, partial, &rest[..ws_len]);
    let sensitive_alone = WindowBoundaryTokens::context_redacts_token("", token);
    let sensitive_with_introducer = WindowBoundaryTokens::context_redacts_token(&prefix, token);
    // The visible lookbehind has to be what makes the token sensitive.
    // Guessing at an introducer beyond the retained window would also drop
    // ordinary tail markers from a long capped log.
    if sensitive_alone || !sensitive_with_introducer {
        return rest.to_string();
    }
    rest[ws_len + token_len..].to_string()
}

/// Line-prefix context that applies to the tail's first whole token.
///
/// `partial` is the cut token's suffix, already removed from the tail.
/// `leading_ws` is the whitespace that separates it from the next token,
/// including a newline that resets context unless the cut token is a
/// pending introducer.
fn tail_token_prefix(lookbehind: &[u8], partial: &str, leading_ws: &str) -> String {
    let mut tokens = WindowBoundaryTokens::default();
    if !lookbehind.is_empty() {
        tokens.push(lookbehind, 0, 0, None);
    }
    let mut extra = Vec::with_capacity(partial.len() + leading_ws.len());
    extra.extend_from_slice(partial.as_bytes());
    extra.extend_from_slice(leading_ws.as_bytes());
    if !extra.is_empty() {
        tokens.push(&extra, lookbehind.len(), 0, None);
    }
    String::from_utf8_lossy(&tokens.context).into_owned()
}

struct CheckoutEvidenceCollector {
    max_lines: usize,
    scan_limit: usize,
    scanned_bytes: usize,
    source_truncated: bool,
    pending_line: String,
    dropping_line: bool,
    commits: Vec<String>,
    seen_commits: HashSet<String>,
    /// Of the observed tokens, the ones a line actually named as the commit
    /// that was checked out — as opposed to a merge parent or a fetched ref
    /// that happened to share the line.
    named_commits: HashSet<String>,
    /// A recognized command whose immediately following output line may name
    /// the checkout. Keep only one bounded line: an unrelated intervening
    /// line consumes it instead of letting a later SHA inherit its authority.
    pending_checkout_command: Option<String>,
    lines: Vec<String>,
    complete: bool,
    display_truncated: bool,
}

impl CheckoutEvidenceCollector {
    fn new(max_lines: usize, scan_limit: usize) -> Self {
        Self {
            max_lines,
            scan_limit,
            scanned_bytes: 0,
            source_truncated: false,
            pending_line: String::new(),
            dropping_line: false,
            commits: Vec::new(),
            seen_commits: HashSet::new(),
            named_commits: HashSet::new(),
            pending_checkout_command: None,
            lines: Vec::new(),
            complete: true,
            display_truncated: false,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        let remaining = self.scan_limit.saturating_sub(self.scanned_bytes);
        let scanned = &chunk[..chunk.len().min(remaining)];
        self.scanned_bytes += scanned.len();
        if scanned.len() < chunk.len() {
            self.source_truncated = true;
            self.complete = false;
        }
        for part in String::from_utf8_lossy(scanned).split_inclusive('\n') {
            if self.dropping_line {
                if part.ends_with('\n') {
                    self.dropping_line = false;
                }
                continue;
            }
            if self.pending_line.len() + part.len() > MAX_CHECKOUT_EVIDENCE_LINE_BYTES {
                let identity_bearing = overlong_line_is_identity_bearing(&self.pending_line, part);
                self.pending_line.clear();
                self.dropping_line = !part.ends_with('\n');
                // A dropped physical line still consumes a preceding
                // `git log` command. Otherwise a later unrelated SHA could
                // inherit authority that belonged only to this line.
                self.pending_checkout_command = None;
                if identity_bearing {
                    self.complete = false;
                } else {
                    self.display_truncated = true;
                }
                continue;
            }
            self.pending_line.push_str(part);
            if self.pending_line.ends_with('\n') {
                self.observe_pending_line();
            }
        }
    }

    fn finish(mut self) -> CheckoutEvidence {
        if !self.pending_line.is_empty() && !self.dropping_line {
            self.observe_pending_line();
        }
        CheckoutEvidence {
            commits: canonical_checkout_commits(&self.commits, &self.named_commits),
            lines: self.lines,
            complete: self.complete,
            scanned_bytes: self.scanned_bytes,
            source_truncated: self.source_truncated,
            display_truncated: self.display_truncated,
        }
    }

    fn observe_pending_line(&mut self) {
        let mut line = std::mem::take(&mut self.pending_line);
        self.observe_line(&line);
        line.clear();
        self.pending_line = line;
    }

    fn observe_line(&mut self, line: &str) {
        // Neither a checkout marker nor a recognized command nor a bare SHA
        // fits below this length. Even an unrelated short line consumes the
        // immediately preceding checkout command's output context.
        if line.len() < MIN_SHA_LEN {
            self.pending_checkout_command = None;
            return;
        }
        let (step, payload) = split_log_line(line);
        let lowered = payload.to_ascii_lowercase();
        let marked = CHECKOUT_MARKERS
            .iter()
            .any(|marker| lowered.contains(marker));
        let in_checkout_step = step.to_ascii_lowercase().contains("checkout");
        let command_output = self
            .pending_checkout_command
            .take()
            .filter(|_| is_bare_commit_sha(payload));
        let command = checkout_identity_command(payload);
        if !(marked || in_checkout_step && is_bare_commit_sha(payload) || command_output.is_some())
        {
            self.pending_checkout_command = command;
            return;
        }
        if let Some(command) = &command_output {
            self.record_evidence_line(command);
        }
        self.record_evidence_line(payload);
        let named = named_checkout_commit(
            payload,
            &lowered,
            in_checkout_step || command_output.is_some(),
        );
        for token in commit_sha_tokens(payload) {
            if named == Some(token) {
                self.named_commits.insert(token.to_string());
            }
            if self.seen_commits.contains(token) {
                continue;
            }
            if self.commits.len() == self.max_lines {
                self.display_truncated = true;
                continue;
            }
            let token = token.to_string();
            self.seen_commits.insert(token.clone());
            self.commits.push(token);
        }
        self.pending_checkout_command = command;
    }

    fn record_evidence_line(&mut self, payload: &str) {
        if self.lines.len() < self.max_lines {
            self.lines.push(redact_all(payload.trim()));
        } else {
            self.display_truncated = true;
        }
    }
}

/// Exclude runner metadata from credential context, retaining the original
/// payload bytes (including invalid UTF-8 and line endings) for byte accounting.
fn runner_log_payload(line: &[u8]) -> &[u8] {
    let line = line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(line);
    let without_timestamp = |bytes: &[u8]| {
        bytes
            .iter()
            .position(|byte| *byte == b' ')
            .filter(|&space| bytes[..space].contains(&b'T') && bytes[..space].ends_with(b"Z"))
    };
    // Raw job-API lines may contain tabs in their payload. Recognize their
    // timestamp before interpreting tabs as job/step column separators.
    if let Some(space) = without_timestamp(line) {
        return &line[space + 1..];
    }
    let mut columns = line.splitn(3, |byte| *byte == b'\t');
    let rest = match (columns.next(), columns.next(), columns.next()) {
        (Some(_job), Some(_step), Some(rest)) => rest,
        _ => line,
    };
    match without_timestamp(rest) {
        Some(space) => &rest[space + 1..],
        None => rest,
    }
}

/// Split one `gh run view --log` line into its step column and its payload.
///
/// The line format is `job<TAB>step<TAB>timestamp payload`. The split matters:
/// the *step* column names a pinned action (`actions/checkout@<sha>`), and
/// harvesting that SHA would report the checkout action's own commit as the
/// commit under test — the exact conflation this scan exists to prevent.
fn split_log_line(line: &str) -> (&str, &str) {
    let line = line.trim_start_matches('\u{feff}');
    let mut columns = line.splitn(3, '\t');
    let (step, rest) = match (columns.next(), columns.next(), columns.next()) {
        (Some(_job), Some(step), Some(rest)) => (step, rest),
        _ => ("", line),
    };
    // Drop the leading ISO-8601 runner timestamp when there is one.
    let payload = match rest.split_once(' ') {
        Some((first, tail)) if first.contains('T') && first.ends_with('Z') => tail,
        _ => rest,
    };
    (step, payload)
}

/// Whether a line dropped for exceeding [`MAX_CHECKOUT_EVIDENCE_LINE_BYTES`]
/// could plausibly have carried checkout identity, judged from whatever
/// prefix was captured before the drop.
///
/// The job/step columns and runner timestamp sit at the very start of a
/// `gh run view --log` line, long before any payload could reach this
/// threshold, so the accumulated prefix (or, when nothing had accumulated yet,
/// a short probe from the start of the new part) is enough to classify it
/// without buffering the whole overlong line.
fn overlong_line_is_identity_bearing(pending: &str, part: &str) -> bool {
    let probe = if pending.is_empty() {
        let cap = ceil_char_boundary(part, part.len().min(512));
        &part[..cap]
    } else {
        pending
    };
    let (step, payload) = split_log_line(probe);
    let lowered = payload.to_ascii_lowercase();
    step.to_ascii_lowercase().contains("checkout")
        || CHECKOUT_MARKERS
            .iter()
            .any(|marker| lowered.contains(marker))
}

fn is_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

/// Whether a payload is a bare commit SHA on its own line.
///
/// `actions/checkout` prints the resolved commit this way, with no surrounding
/// prose, which is often the only place a branch-mode checkout records what it
/// actually landed on.
fn is_bare_commit_sha(payload: &str) -> bool {
    let trimmed = payload.trim();
    trimmed.len() == MAX_SHA_LEN && trimmed.bytes().all(is_hex)
}

/// Recognize the runner command that prints the checked-out `HEAD` as its
/// next line. This intentionally accepts only `git log -1 --format=%H`, not
/// a generic bare SHA after any command: ordinary test output and arbitrary
/// repository probes must not become checkout identity.
fn checkout_identity_command(payload: &str) -> Option<String> {
    let command = payload.strip_prefix("[command]")?.trim();
    let mut words = command.split_ascii_whitespace();
    let program = words.next()?;
    if program.rsplit('/').next()? != "git"
        || !matches!(words.next(), Some("log"))
        || !matches!(words.next(), Some("-1"))
        || !matches!(words.next(), Some("--format=%H"))
        || words.next().is_some()
    {
        return None;
    }
    Some(command.to_string())
}

/// The prose `actions/checkout` prints in front of the commit it landed on.
const HEAD_IS_NOW_AT: &str = "head is now at";

/// The commit this line *names* as checked out, if it names one.
///
/// `HEAD is now at 7dcd45b Merge 4968f13 into abc1234` reports one checked-out
/// commit and then quotes the merge's two parents inside the commit subject.
/// Only the token directly after the marker is identity; the rest is prose
/// that happens to be hex. Everything else — a fetched ref, a merge summary —
/// names no commit and stays incidental evidence.
fn named_checkout_commit<'a>(
    payload: &'a str,
    lowered: &str,
    in_checkout_step: bool,
) -> Option<&'a str> {
    if let Some(marker) = lowered.find(HEAD_IS_NOW_AT) {
        // `to_ascii_lowercase` rewrites ASCII bytes in place and leaves every
        // multi-byte sequence alone, so offsets into `lowered` index `payload`.
        return commit_sha_tokens(&payload[marker + HEAD_IS_NOW_AT.len()..])
            .into_iter()
            .next();
    }
    (in_checkout_step && is_bare_commit_sha(payload)).then(|| payload.trim())
}

/// Reduce the observed SHA tokens to the distinct commits actually checked out.
///
/// Two reductions, neither of which relaxes identity. A line that *names* the
/// checked-out commit outranks one that merely quotes a merge parent or a
/// fetched ref. An abbreviation and the full SHA it prefixes are one commit,
/// not two — the same runner routinely prints both forms. A real disagreement
/// between two checkout steps survives both reductions and is still reported
/// as two commits, and an abbreviation with more than one distinct expansion
/// is left abbreviated rather than resolved to a guess.
fn canonical_checkout_commits(observed: &[String], named: &HashSet<String>) -> Vec<String> {
    let identity: Vec<&String> = if observed.iter().any(|commit| named.contains(commit)) {
        observed
            .iter()
            .filter(|commit| named.contains(*commit))
            .collect()
    } else {
        observed.iter().collect()
    };

    let mut canonical: Vec<String> = Vec::new();
    for commit in identity {
        // `observed` holds distinct tokens, so two expansions of the same
        // length are two different commits and the abbreviation cannot be
        // resolved to either of them.
        let mut longest: Option<&String> = None;
        let mut contested = false;
        for candidate in observed
            .iter()
            .filter(|candidate| candidate.len() > commit.len() && candidate.starts_with(&**commit))
        {
            match longest {
                Some(current) if current.len() > candidate.len() => {}
                Some(current) if current.len() == candidate.len() => contested = true,
                _ => {
                    longest = Some(candidate);
                    contested = false;
                }
            }
        }
        let resolved = if contested {
            commit
        } else {
            longest.unwrap_or(commit)
        };
        if !canonical.contains(resolved) {
            canonical.push(resolved.clone());
        }
    }
    canonical
}

/// Collect every lowercase-hex token of commit-SHA length in `payload`.
///
/// A token preceded by `@` is skipped: that is the `owner/action@sha` pin
/// form, which identifies the action, not the commit under test.
fn commit_sha_tokens(payload: &str) -> Vec<&str> {
    let bytes = payload.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if !bytes[index].is_ascii_alphanumeric() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_alphanumeric() {
            index += 1;
        }
        let token = &payload[start..index];
        let pinned = start > 0 && bytes[start - 1] == b'@';
        if !pinned
            && (MIN_SHA_LEN..=MAX_SHA_LEN).contains(&token.len())
            && token.bytes().all(is_hex)
        {
            tokens.push(token);
        }
    }
    tokens
}
