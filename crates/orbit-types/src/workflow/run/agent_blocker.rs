//! An implementer-declared blocker [ORB-14269].
//!
//! The implementer returns `blocker: {kind, evidence}` on a successful
//! protocol envelope. The step boundary turns a well-formed value into the
//! [`TASK_BLOCKED_BY_AGENT_ERROR_CODE`] failure. Step retry, step recovery,
//! and final recovery skip it. The failure handoff blocks the task under
//! [`TASK_BLOCKED_BY_AGENT_EVENT`] and leaves the worktree uncommitted,
//! with no `[BLOCKED]` pull request.
//!
//! An implementer that instead returns a `status: "failed"` envelope whose
//! `error.code` names a task-level stop is normalized the same way at the
//! step boundary ([`agent_blocker_from_declared_failure`]): the code is the
//! kind and the error message is the evidence. Codes Orbit already reserves
//! for another outcome (provider, transient, typed holds and
//! `implementation_incomplete`) keep their existing paths.
//!
//! A malformed shape is not a blocker. The step keeps its ordinary outcome.
//!
//! Step recovery declares the same blocker through an `external_blocker`
//! decision [ORB-14268]; its step fails with the same marker and handoff.
//!
//! A kind is classified once, here ([`AgentBlockerClass::of_kind`]). A
//! blocker that says an Orbit upgrade refused the agent's own `orbit`
//! commands mid-step describes the host, not the task: its step fails under
//! [`UPGRADE_PENDING_ERROR_CODE`] instead, which settles as transient, keeps
//! the candidate and returns the task to the backlog, like a run interrupted
//! at a step boundary by the same upgrade.

use super::failure_classification_text;
use crate::task::{TaskDecision, TaskHistoryEntry};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Error code of an implementer-declared blocker.
pub const TASK_BLOCKED_BY_AGENT_ERROR_CODE: &str = "task_blocked_by_agent";

/// The bracketed marker form of [`TASK_BLOCKED_BY_AGENT_ERROR_CODE`].
pub const TASK_BLOCKED_BY_AGENT_MARKER: &str = "[task_blocked_by_agent]";

/// Task history event that blocks a task for an implementer-declared blocker.
///
/// Resume does not treat this event as a failure handoff it may undo. An
/// operator moves the task back when the blocker is gone.
pub const TASK_BLOCKED_BY_AGENT_EVENT: &str = "task_blocked_by_agent";

/// Error code of a step whose agent was refused by an Orbit upgrade
/// mid-step: a newer `orbit` replaced the installed one and would switch the
/// store generation, which a command inside a running step never does.
pub const UPGRADE_PENDING_ERROR_CODE: &str = "upgrade_pending";

/// The bracketed marker form of [`UPGRADE_PENDING_ERROR_CODE`]. The
/// generation admission's refusal to a command inside a managed activity
/// carries it, and so does the step failure.
pub const UPGRADE_PENDING_MARKER: &str = "[upgrade_pending]";

/// Task history event that returns an owner's task to the backlog after its
/// run's agent was refused by an Orbit upgrade.
pub const UPGRADE_PENDING_REQUEUED_EVENT: &str = "upgrade_pending_requeued";

/// Largest `evidence` string kept on the step failure. A longer value is
/// cut on a char boundary so a real blocker is not dropped for size.
const MAX_EVIDENCE_BYTES: usize = 4 * 1024;

/// A well-formed implementer blocker.
///
/// The failure handoff records it on the history entry it writes, as
/// [`TaskDecision::AgentBlocked`], and readers take its kind from there
/// through [`TaskHistoryEntry::agent_blocker_kind`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentBlocker {
    /// Short token naming the blocker.
    pub kind: String,
    /// Why the implementer stopped, bounded to `MAX_EVIDENCE_BYTES`.
    pub evidence: String,
}

/// What a blocker's kind says stopped the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentBlockerClass {
    /// The task needs a human before it continues. Every kind not named
    /// below.
    Task,
    /// An Orbit upgrade refused the agent's own `orbit` commands: the host
    /// is mid-upgrade, and the step can run again once it settles.
    UpgradePending,
}

impl AgentBlockerClass {
    /// Classify a kind token.
    ///
    /// Agents spell an upgrade refusal many ways (`upgrade_pending`,
    /// `upgrade_admission_refused`, `orbit_upgrade_admission_refused`,
    /// `orbit.generation_switch_pending`), so the kind is read by its words:
    /// split on `_`, `.`, `:` and `-`, case-insensitively. `upgrade` with
    /// `admission`, `pending` or `quiesce`, or `generation` with `switch`,
    /// is an upgrade refusal.
    #[must_use]
    pub fn of_kind(kind: &str) -> Self {
        let words: Vec<String> = kind
            .split(['_', '.', ':', '-'])
            .map(str::to_ascii_lowercase)
            .collect();
        let has = |word: &str| words.iter().any(|candidate| candidate == word);
        if (has("upgrade") && (has("admission") || has("pending") || has("quiesce")))
            || (has("generation") && has("switch"))
        {
            Self::UpgradePending
        } else {
            Self::Task
        }
    }
}

impl AgentBlocker {
    /// The class of this blocker's kind.
    #[must_use]
    pub fn class(&self) -> AgentBlockerClass {
        AgentBlockerClass::of_kind(&self.kind)
    }

    /// The blocker a [`step_failure_message`](Self::step_failure_message)
    /// carries, behind either marker: the `kind=` token, and the text after
    /// it as evidence. `None` when no well-formed kind or no evidence
    /// follows the marker.
    #[must_use]
    pub fn from_step_failure_message(message: &str) -> Option<Self> {
        let (kind, evidence) = kind_and_rest_after(message, TASK_BLOCKED_BY_AGENT_MARKER)
            .or_else(|| kind_and_rest_after(message, UPGRADE_PENDING_MARKER))?;
        let evidence = evidence.trim();
        (!evidence.is_empty()).then(|| Self {
            kind: kind.to_string(),
            evidence: bounded_text(evidence, MAX_EVIDENCE_BYTES),
        })
    }

    /// The step-failure text for this blocker: [`task_blocked_by_agent_message`]
    /// for a task blocker, the [`UPGRADE_PENDING_MARKER`] form for an upgrade
    /// refusal. The marker and `kind=` lead either way.
    #[must_use]
    pub fn step_failure_message(&self) -> String {
        match self.class() {
            AgentBlockerClass::Task => task_blocked_by_agent_message(self),
            AgentBlockerClass::UpgradePending => format!(
                "{UPGRADE_PENDING_MARKER} kind={}\n{}",
                self.kind, self.evidence
            ),
        }
    }
}

/// Whether a step failure says an Orbit upgrade refused its agent mid-step.
#[must_use]
pub fn is_upgrade_pending(error_code: Option<&str>, message: Option<&str>) -> bool {
    error_code == Some(UPGRADE_PENDING_ERROR_CODE)
        || message.is_some_and(|message| {
            failure_classification_text(message).contains(UPGRADE_PENDING_MARKER)
        })
}

/// Whether a step failure says an agent (the implementer or step recovery)
/// declared a blocker.
#[must_use]
pub fn is_task_blocked_by_agent(error_code: Option<&str>, message: Option<&str>) -> bool {
    error_code == Some(TASK_BLOCKED_BY_AGENT_ERROR_CODE)
        || message.is_some_and(|message| {
            failure_classification_text(message).contains(TASK_BLOCKED_BY_AGENT_MARKER)
        })
}

/// The blocker an activity output declares, when `{kind, evidence}` is well formed.
///
/// `kind` is a token of at most 64 bytes: an ASCII letter, then ASCII
/// alphanumeric, `_`, `.`, `:`, or `-`. `evidence` is non-empty after trim.
/// Anything else, including a missing field, is `None`.
#[must_use]
pub fn agent_blocker_from_output(output: &Value) -> Option<AgentBlocker> {
    let blocker = output.get("blocker")?;
    let kind = blocker.get("kind").and_then(Value::as_str)?.trim();
    if !is_blocker_kind(kind) {
        return None;
    }
    let evidence = blocker.get("evidence").and_then(Value::as_str)?.trim();
    if evidence.is_empty() {
        return None;
    }
    Some(AgentBlocker {
        kind: kind.to_string(),
        evidence: bounded_text(evidence, MAX_EVIDENCE_BYTES),
    })
}

/// `error.code` values a declared-failed envelope may carry that already
/// name an Orbit outcome, so they are never re-read as a blocker kind.
/// This is the exclusion list the step boundary applies before
/// [`agent_blocker_from_declared_failure`]: the typed step codes
/// (`owner_route_unavailable`, `owner_signoff_required`,
/// `footprint_unavailable`), the provider classes (`provider_unavailable`,
/// `provider_limit`, `provider_capacity`, `provider_refusal`), the
/// validation-environment pair, the baseline-red and forge holds,
/// `transient_failure` and `stalled`, the image-input codes, the held-run
/// codes, the synthesized CLI terminal codes, `task_blocked_by_agent` and
/// `upgrade_pending` (both already hold under their own classification),
/// `worktree_mismatch` (the envelope's own mismatch failure), and
/// `implementation_incomplete` (its bounded continuation path).
const RESERVED_DECLARED_FAILURE_CODES: &[&str] = &[
    "implementation_incomplete",
    TASK_BLOCKED_BY_AGENT_ERROR_CODE,
    UPGRADE_PENDING_ERROR_CODE,
    super::state::OWNER_ROUTE_UNAVAILABLE_ERROR_CODE,
    super::state::OWNER_SIGNOFF_REQUIRED_ERROR_CODE,
    super::state::FOOTPRINT_UNAVAILABLE_ERROR_CODE,
    super::state::PROVIDER_UNAVAILABLE_ERROR_CODE,
    super::state::PROVIDER_LIMIT_ERROR_CODE,
    super::state::PROVIDER_CAPACITY_ERROR_CODE,
    super::state::PROVIDER_REFUSAL_ERROR_CODE,
    super::state::VALIDATION_ENVIRONMENT_ERROR_CODE,
    super::state::VALIDATION_ROUTE_UNAVAILABLE_ERROR_CODE,
    super::state::TRANSIENT_FAILURE_ERROR_CODE,
    super::state::STALLED_ERROR_CODE,
    super::baseline::BASELINE_RED_ERROR_CODE,
    super::forge_hold::FORGE_UNAVAILABLE_ERROR_CODE,
    crate::workflow::IMAGE_INPUT_UNAVAILABLE_ERROR_CODE,
    crate::workflow::IMAGE_NOT_SEEN_ERROR_CODE,
    super::root_cause::REVIEW_DECISION_PENDING_ERROR_CODE,
    super::root_cause::CHILD_TIMEOUT_ERROR_CODE,
    super::root_cause::OPERATOR_STOPPED_ERROR_CODE,
    "AGENT_TIMEOUT",
    "AGENT_INVOCATION_FAILED",
    "AGENT_TERMINAL_ENDING",
    "worktree_mismatch",
];

/// Whether `code` is reserved for an existing Orbit outcome and so keeps
/// that outcome when a declared-failed envelope carries it.
#[must_use]
pub fn is_reserved_declared_failure_code(code: &str) -> bool {
    RESERVED_DECLARED_FAILURE_CODES.contains(&code)
}

/// Words that make a declared-failed `error.code` name a task-level stop.
/// The code is split on `_ . : -` (as [`AgentBlockerClass::of_kind`] does)
/// and must contain one of them as a whole word, so `validation_blocker`,
/// `task_blocked`, `contract_contradiction` and `live_fixture_unavailable`
/// hold while an ordinary failure code such as `red` or `example_failure`
/// stays a failure.
pub const DECLARED_BLOCKER_CODE_WORDS: &[&str] =
    &["blocker", "blocked", "contradiction", "unavailable"];

/// Whether `code`, split on `_ . : -`, contains a
/// [`DECLARED_BLOCKER_CODE_WORDS`] word.
fn names_declared_blocker_word(code: &str) -> bool {
    code.split(['_', '.', ':', '-'])
        .any(|word| DECLARED_BLOCKER_CODE_WORDS.contains(&word.to_ascii_lowercase().as_str()))
}

/// The blocker a declared-failed implementer envelope names. All three must
/// hold, in this order: the `error.code` is not reserved for another outcome
/// (see [`is_reserved_declared_failure_code`]: provider, transient, typed
/// holds and `implementation_incomplete`); it is a well-formed kind token
/// containing a [`DECLARED_BLOCKER_CODE_WORDS`] word; and the
/// `error.message` is non-empty evidence. The code is the kind, classified
/// once through [`AgentBlockerClass::of_kind`] by the caller, and the message
/// is the evidence, bounded like a `blocker` value's. Anything else is
/// `None`, and the step keeps its ordinary failed outcome.
#[must_use]
pub fn agent_blocker_from_declared_failure(code: &str, evidence: &str) -> Option<AgentBlocker> {
    let kind = code.trim();
    if is_reserved_declared_failure_code(kind)
        || !is_blocker_kind(kind)
        || !names_declared_blocker_word(kind)
    {
        return None;
    }
    let evidence = evidence.trim();
    if evidence.is_empty() {
        return None;
    }
    Some(AgentBlocker {
        kind: kind.to_string(),
        evidence: bounded_text(evidence, MAX_EVIDENCE_BYTES),
    })
}

/// The step-failure text for `blocker`. The marker and `kind=` lead, so a
/// later note that keeps only the head of the message still classifies.
#[must_use]
pub fn task_blocked_by_agent_message(blocker: &AgentBlocker) -> String {
    format!(
        "{TASK_BLOCKED_BY_AGENT_MARKER} kind={}\n{}",
        blocker.kind, blocker.evidence
    )
}

/// The kind token a failure message or history note carries, if any.
#[must_use]
pub fn task_blocked_by_agent_kind(message: &str) -> Option<&str> {
    kind_after(message, TASK_BLOCKED_BY_AGENT_MARKER)
}

/// The kind token an [`UPGRADE_PENDING_MARKER`] failure carries, if any.
#[must_use]
pub fn upgrade_pending_kind(message: &str) -> Option<&str> {
    kind_after(message, UPGRADE_PENDING_MARKER)
}

impl TaskHistoryEntry {
    /// The kind of the agent blocker this entry records.
    ///
    /// An entry written since [`TaskDecision::AgentBlocked`] carries the
    /// blocker typed. A [`TASK_BLOCKED_BY_AGENT_EVENT`] entry written before
    /// it (or by an older binary or replica) names the kind only in its note,
    /// `[task_blocked_by_agent] kind=<kind> …`, which new entries keep
    /// writing byte for byte; the legacy note reader reads that.
    #[must_use]
    pub fn agent_blocker_kind(&self) -> Option<&str> {
        match &self.decision {
            Some(TaskDecision::AgentBlocked { blocker }) => Some(&blocker.kind),
            Some(_) => None,
            None if self.event == TASK_BLOCKED_BY_AGENT_EVENT => {
                task_blocked_by_agent_kind(self.note.as_deref()?)
            }
            None => None,
        }
    }
}

fn kind_after<'a>(message: &'a str, marker: &str) -> Option<&'a str> {
    kind_and_rest_after(message, marker).map(|(kind, _)| kind)
}

/// The `kind=` token after `marker`, and the text that follows it.
fn kind_and_rest_after<'a>(message: &'a str, marker: &str) -> Option<(&'a str, &'a str)> {
    let message = failure_classification_text(message);
    let (_, rest) = message.split_once(marker)?;
    let rest = rest.trim_start().strip_prefix("kind=")?.trim_start();
    let kind = rest.split_whitespace().next()?;
    is_blocker_kind(kind).then(|| (kind, &rest[kind.len()..]))
}

fn is_blocker_kind(kind: &str) -> bool {
    let mut chars = kind.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() || kind.len() > 64 {
        return false;
    }
    kind.chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | ':' | '-'))
}

fn bounded_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}
