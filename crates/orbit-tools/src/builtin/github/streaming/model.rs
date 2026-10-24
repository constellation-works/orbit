use serde_json::Value;

/// Prose a runner emits around a checkout. Matched case-insensitively.
pub(super) const CHECKOUT_MARKERS: &[&str] = &[
    "head is now at",
    "-> fetch_head",
    "git checkout --progress",
    "checking out the ref",
    "checkout ref",
];

pub(super) const MIN_SHA_LEN: usize = 7;
pub(super) const MAX_SHA_LEN: usize = 40;
/// The most source bytes checkout extraction will inspect from one log. The
/// rest is still drained so `gh` can exit, but identity is marked incomplete.
pub const MAX_CHECKOUT_LOG_SCAN_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_CHECKOUT_EVIDENCE_LINE_BYTES: usize = 16 * 1024;
/// Bound and amortize work across small transport reads.
pub(super) const STREAM_BATCH_BYTES: usize = 4096;
/// Original line-prefix/token context for selection cuts. Context beyond
/// this bound is conservatively omitted rather than retaining a credential.
pub(super) const WINDOW_TOKEN_CONTEXT_BYTES: usize = 16 * 1024;

/// The commit a runner actually checked out, read out of the runner's own log.
///
/// A workflow event's reported head SHA is metadata; this is evidence. They
/// disagree whenever a merge-queue or pull-request merge commit is what got
/// tested, so the two travel as separate fields and are never merged into one
/// `sha`.
pub struct CheckoutEvidence {
    /// The distinct commits the log named as checked out, with an
    /// abbreviation collapsed into the full SHA it prefixes. A checkout into
    /// a directory nested under another checkout directory in the same log is
    /// omitted: that is a side-path helper, not the workspace under test.
    /// More than one remaining entry means those checkouts genuinely disagreed.
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

    pub(super) fn needs_line_mode(&self) -> bool {
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
