//! An implementer-declared blocker [ORB-14269].
//!
//! The implementer returns `blocker: {kind, evidence}` on a successful
//! protocol envelope. The step boundary turns a well-formed value into the
//! [`TASK_BLOCKED_BY_AGENT_ERROR_CODE`] failure. Step retry, step recovery,
//! and final recovery skip it. The failure handoff blocks the task under
//! [`TASK_BLOCKED_BY_AGENT_EVENT`] and leaves the worktree uncommitted,
//! with no `[BLOCKED]` pull request.
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
        || message.is_some_and(|message| message.contains(UPGRADE_PENDING_MARKER))
}

/// Whether a step failure says an agent (the implementer or step recovery)
/// declared a blocker.
#[must_use]
pub fn is_task_blocked_by_agent(error_code: Option<&str>, message: Option<&str>) -> bool {
    error_code == Some(TASK_BLOCKED_BY_AGENT_ERROR_CODE)
        || message.is_some_and(|message| message.contains(TASK_BLOCKED_BY_AGENT_MARKER))
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

fn kind_after<'a>(message: &'a str, marker: &str) -> Option<&'a str> {
    let (_, rest) = message.split_once(marker)?;
    let rest = rest.trim_start().strip_prefix("kind=")?;
    let kind = rest.split_whitespace().next()?;
    is_blocker_kind(kind).then_some(kind)
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
