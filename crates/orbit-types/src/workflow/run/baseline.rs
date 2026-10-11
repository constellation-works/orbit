//! A required validation command that fails on the candidate's base exactly
//! as it fails on the candidate [ORB-14258].
//!
//! The integration branch has no merge gates of its own, so a check can be red
//! on the base every delivery starts from. A candidate that inherits that
//! failure did not cause it, and no repair of the candidate fixes it: step and
//! final recovery skip it, no `[BLOCKED]` PR is opened, and the task is held in
//! the backlog (a claimed leaf releases its claim) until the base moves to a
//! commit where the command passes.
//!
//! The failure text and the hold's task-history note carry the same typed
//! [`BaselineRedHold`], written as JSON straight after
//! [`BASELINE_RED_MARKER`], so every consumer — failure handoff, run
//! finalization, claim settlement and admission — reads the hold from the
//! text it already has.
//!
//! [ORB-15374] The hold also names what fails on the base
//! ([`BaselineRedFailures`]), so the hold text, its verdicts and the
//! readiness detail say what is red, and the owner can file one repair task
//! per red base failure.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Error code of a required validation failure the base shares.
pub const BASELINE_RED_ERROR_CODE: &str = "baseline_red";

/// The bracketed marker form of [`BASELINE_RED_ERROR_CODE`].
pub const BASELINE_RED_MARKER: &str = "[baseline_red]";

/// Task history event that moves a task to `backlog` under a
/// [`BaselineRedHold`]. Admission withholds the task while the hold stands.
pub const BASELINE_RED_HOLD_EVENT: &str = "baseline_red_hold";

/// Whether a step failure says its required validation is red on the base.
#[must_use]
pub fn is_baseline_red_failure(error_code: Option<&str>, message: Option<&str>) -> bool {
    error_code == Some(BASELINE_RED_ERROR_CODE)
        || message.is_some_and(|message| message.contains(BASELINE_RED_MARKER))
}

/// Which base a required command failed on, and the command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineRedHold {
    /// The ref the run synchronized onto (`origin/agent-main`, say). The hold
    /// lifts only after this ref points at a new commit where the command
    /// passes. Empty when the run did not report it; admission keeps the hold
    /// until the base ref can be resolved.
    #[serde(default)]
    pub base_ref: String,
    /// The base commit the command failed on.
    pub base_sha: String,
    /// The required command, as configured.
    pub command: String,
    /// The run that observed the failure.
    #[serde(default)]
    pub run_id: String,
    /// What the failing run reported testing, for a command that selects its
    /// own tests [ORB-15131]. A moved base is checked on this selection, so a
    /// tip that would select nothing of its own cannot lift the hold. Absent
    /// for a command that reports no selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<serde_json::Value>,
    /// What the base run named as failing [ORB-15374]. Empty for a hold
    /// recorded before holds kept it.
    #[serde(default, skip_serializing_if = "BaselineRedFailures::is_empty")]
    pub failures: BaselineRedFailures,
}

/// Most failure identities a hold displays; the rest are counted and hashed.
const MAX_BASELINE_RED_FAILURES: usize = 20;
/// Lines of output a hold keeps when the base run names no failure.
const BASELINE_RED_TAIL_LINES: usize = 5;
/// Longest failure identity or output line a hold keeps, in bytes.
const MAX_BASELINE_RED_LINE_BYTES: usize = 240;
/// Failures the one-line description names before counting the rest.
const DESCRIBED_FAILURES: usize = 5;

/// What a red base run named as failing, bounded so the persisted hold stays
/// small [ORB-15374].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineRedFailures {
    /// SHA-256 of the complete normalized identity set, before display bounds.
    /// Absent for older holds and output-only failures. Repair deduplication
    /// uses this digest so omitted or shortened identities remain distinct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_digest: Option<String>,
    /// Each failure the run's output names, sorted: a failed test
    /// (`test <binary-id> <test>` for nextest, `test <name>` for libtest) or
    /// a compiler or lint error at its location. At most 20.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identities: Vec<String>,
    /// Identities the run named beyond those kept.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted: usize,
    /// The last non-blank lines of the output, at most 5, kept only when it
    /// names no failure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tail: Vec<String>,
}

fn is_zero(count: &usize) -> bool {
    *count == 0
}

impl BaselineRedFailures {
    /// The failures `identities` names, as a run's output parses them, or
    /// the tail of `output` when there are none.
    ///
    /// nextest also echoes each failed test's libtest line, whose bare name
    /// the qualified `test <binary-id> <test>` identity already covers, so
    /// the bare one is dropped.
    #[must_use]
    pub fn new(identities: impl IntoIterator<Item = String>, output: &str) -> Self {
        let identities = identities
            .into_iter()
            .map(|identity| identity.trim().to_string())
            .filter(|identity| !identity.is_empty())
            .collect::<std::collections::BTreeSet<_>>();
        let qualified = identities
            .iter()
            .filter_map(|identity| identity.strip_prefix("test ")?.split_once(' '))
            .map(|(_, test)| format!("test {test}"))
            .collect::<std::collections::BTreeSet<_>>();
        let identities = identities
            .into_iter()
            .filter(|identity| !qualified.contains(identity))
            .collect::<Vec<_>>();
        if identities.is_empty() {
            return Self {
                tail: output_tail(output),
                ..Self::default()
            };
        }
        // Frame each sorted identity by its byte length: embedded delimiters
        // cannot make two different sets share the same digest input.
        let mut digest = Sha256::new();
        digest.update(b"baseline-red-failures-v1\0");
        for identity in &identities {
            digest.update((identity.len() as u64).to_be_bytes());
            digest.update(identity.as_bytes());
        }
        let omitted = identities.len().saturating_sub(MAX_BASELINE_RED_FAILURES);
        Self {
            identity_digest: Some(format!("{:x}", digest.finalize())),
            identities: identities
                .into_iter()
                .take(MAX_BASELINE_RED_FAILURES)
                .map(|identity| bounded_line(&identity))
                .collect(),
            omitted,
            tail: Vec::new(),
        }
    }

    /// Whether the run named nothing, or the hold predates the record.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.identities.is_empty() && self.tail.is_empty()
    }

    /// One line naming the failures (`failing: ...`), or `None` when there
    /// are none.
    #[must_use]
    pub fn describe(&self) -> Option<String> {
        if !self.identities.is_empty() {
            let shown = self
                .identities
                .iter()
                .take(DESCRIBED_FAILURES)
                .map(|identity| format!("`{}`", identity.strip_prefix("test ").unwrap_or(identity)))
                .collect::<Vec<_>>()
                .join(", ");
            let more = self.identities.len().saturating_sub(DESCRIBED_FAILURES) + self.omitted;
            return Some(if more == 0 {
                format!("failing: {shown}")
            } else {
                format!("failing: {shown} and {more} more")
            });
        }
        (!self.tail.is_empty()).then(|| {
            format!(
                "no failing test named; the output ends {}",
                self.tail
                    .iter()
                    .map(|line| format!("`{line}`"))
                    .collect::<Vec<_>>()
                    .join(" / ")
            )
        })
    }
}

/// The last [`BASELINE_RED_TAIL_LINES`] non-blank lines of `output`, color
/// codes dropped and each line bounded.
fn output_tail(output: &str) -> Vec<String> {
    let lines = output
        .lines()
        .map(|line| strip_ansi(line).trim().to_string())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    lines[lines.len().saturating_sub(BASELINE_RED_TAIL_LINES)..]
        .iter()
        .map(|line| bounded_line(line))
        .collect()
}

/// `text` without its ANSI escape sequences (`ESC [ ... <letter>`).
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        if chars.next() == Some('[') {
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

/// `line` cut to [`MAX_BASELINE_RED_LINE_BYTES`] on a character boundary.
fn bounded_line(line: &str) -> String {
    if line.len() <= MAX_BASELINE_RED_LINE_BYTES {
        return line.to_string();
    }
    let mut cut = MAX_BASELINE_RED_LINE_BYTES;
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &line[..cut])
}

impl BaselineRedHold {
    /// `detail` prefixed with the marker and this hold, the form both the
    /// step failure and the hold note use.
    #[must_use]
    pub fn text(&self, detail: &str) -> String {
        let hold = serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string());
        format!("{BASELINE_RED_MARKER} {hold} {}", detail.trim())
    }

    /// `; failing: ...` naming the hold's failures, or nothing for a hold
    /// that records none, to end a sentence about the hold.
    #[must_use]
    pub fn failures_clause(&self) -> String {
        self.failures
            .describe()
            .map(|described| format!("; {described}"))
            .unwrap_or_default()
    }

    /// The hold a failure message or history note carries, if any.
    #[must_use]
    pub fn from_text(text: &str) -> Option<Self> {
        let (_, rest) = text.split_once(BASELINE_RED_MARKER)?;
        serde_json::Deserializer::from_str(rest.trim_start())
            .into_iter::<Self>()
            .next()?
            .ok()
            .filter(|hold| !hold.base_sha.trim().is_empty() && !hold.command.trim().is_empty())
    }
}
