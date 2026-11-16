//! One-line reader text for a `source repair refused:` diagnostic.
//!
//! The stored error keeps the full refusal, including every remedy spelling.
//! Task notes, the CLI and MCP show [`source_repair_reader_text`]'s line and
//! keep that stored text as detail.

use super::OrbitError;

/// The prefix every source-repair job-validation refusal carries.
const PREFIX: &str = "source repair refused: ";

/// Display class prefixes that may sit in front of [`PREFIX`].
const CLASS_PREFIXES: [&str; 3] = [
    "job validation failed: ",
    "invalid input: ",
    "execution failed: ",
];

/// Where a refusal's own sentence stops and the requeue wrapper's essay begins.
const WRAPPER_MARKERS: [&str; 2] = [". Nothing was changed", ". The requeue resumes"];

/// Remedy menus inside a cause. The cut keeps what failed and why.
const MENU_MARKERS: [&str; 6] = [
    "; stop any newer",
    "; inspect explicit",
    ", or discard",
    "; or discard",
    ", or requeue",
    "; or requeue",
];

/// A source-repair refusal as a reader should see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRepairReaderText {
    /// One line: what failed, why, and one next step.
    pub line: String,
    /// The text this line was read from, unchanged.
    pub detail: String,
}

/// The one-line form of `stored` when it is a source-repair refusal.
///
/// `stored` is the text a surface already shows (`Display`, or an HTTP
/// `error` string). Returns `None` when that text is not a
/// `source repair refused:` refusal. The line keeps the prefix. `detail` is
/// `stored`, so a caller can put the previous text beside the line.
pub fn source_repair_reader_text(stored: &str) -> Option<SourceRepairReaderText> {
    let original = stored.trim();
    let mut body = original;
    for class_prefix in CLASS_PREFIXES {
        if let Some(rest) = body.strip_prefix(class_prefix) {
            body = rest.trim_start();
            break;
        }
    }
    let after = body.strip_prefix(PREFIX)?;
    let cause_end = wrapper_end(after);
    let raw = collapse_ws(&after[..cause_end]);
    let (cut_cause, cut) = cut_menu(&raw);
    let mut cause = rewrite_spec_changed(&cut_cause);
    if cause.is_empty() {
        cause = "the source repair was refused".to_string();
    }
    let sentence = if !cut && cause_already_guides(&raw) {
        ensure_period(cause)
    } else {
        format!("{} {}", ensure_period(cause), next_step(&raw))
    };
    let line = collapse_ws(&format!("{PREFIX}{sentence}"));
    Some(SourceRepairReaderText {
        line,
        detail: original.to_string(),
    })
}

impl OrbitError {
    /// The one-line source-repair refusal for this error, when it is one.
    ///
    /// Other errors return `None`. The error's display text is unchanged.
    pub fn source_repair_reader_text(&self) -> Option<SourceRepairReaderText> {
        source_repair_reader_text(&self.to_string())
    }
}

fn wrapper_end(after: &str) -> usize {
    WRAPPER_MARKERS
        .iter()
        .filter_map(|marker| after.find(marker))
        .chain(after.find('\n'))
        .min()
        .unwrap_or(after.len())
}

fn cut_menu(raw: &str) -> (String, bool) {
    let lower = raw.to_ascii_lowercase();
    let cut_at = MENU_MARKERS
        .iter()
        .filter_map(|marker| lower.find(marker))
        .min();
    match cut_at {
        Some(at) if at > 0 => (raw[..at].trim().to_string(), true),
        _ => (raw.to_string(), false),
    }
}

/// The owner's example: the spec changed after PR #N was published.
fn rewrite_spec_changed(cause: &str) -> String {
    let lower = cause.to_ascii_lowercase();
    if !lower.contains("specification changed") {
        return cause.to_string();
    }
    let marker = "pull request #";
    let Some(idx) = lower.find(marker) else {
        return cause.to_string();
    };
    let rest = &cause[idx + marker.len()..];
    let number: String = rest
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '?')
        .collect();
    if number.is_empty() {
        return cause.to_string();
    }
    format!("the spec changed after PR #{number} was published")
}

/// A cause that already names one command, so the line must not add another.
fn cause_already_guides(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    lower.contains("`orbit ")
        || lower.contains("cancel it with")
        || lower.contains("inspect it explicitly")
}

fn next_step(raw: &str) -> &'static str {
    let cause = raw.to_ascii_lowercase();
    if cause.contains("specification changed") || cause.contains("no longer implements") {
        return "Discard the candidate to rerun fresh.";
    }
    // A still-running cause without a cancel command is a terminal record whose
    // worker is not gone: cancel reports already terminal, so only discard can
    // succeed. A non-terminal cause names its cancel command, which the reader
    // keeps as the single next step (`cause_already_guides`).
    if cause.contains("still running") || cause.contains("still live") {
        return "Discard the candidate to rerun fresh.";
    }
    // The requeue resumes only an open pull request, so a closed one names that
    // precondition rather than the checks alone.
    if cause.contains("is not open") {
        return "Requeue when the pull request is open and its required checks pass.";
    }
    if cause.contains("required checks failed")
        || cause.contains("have not finished")
        || cause.contains("no required check")
        || cause.contains("describe head")
        || cause.contains("required checks unreadable")
    {
        return "Requeue when the required checks pass.";
    }
    if cause.contains("closed/merged") || cause.contains("merge intent") {
        return "Settle the task if the pull request merged.";
    }
    if cause.contains("conflicts with its base") {
        return "Resolve the pull request conflict, then requeue.";
    }
    if cause.contains("explicit source repair") || cause.contains("stop any newer") {
        return "Inspect the source again before retrying.";
    }
    // A past-tense discard is a fact the operator already acted on, not a step.
    if cause.contains("discard") && !cause.contains("discarded") {
        return "Discard the candidate to rerun fresh.";
    }
    "Inspect the source again before retrying."
}

fn ensure_period(text: String) -> String {
    if text.ends_with(['.', '!', '?']) {
        text
    } else {
        format!("{text}.")
    }
}

fn collapse_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
