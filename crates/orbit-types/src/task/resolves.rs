//! The frictions a task names, and the retry work its arrival at done leaves.
//!
//! Every write that moves a task into done commits one pending entry per named
//! target with the transition itself. A crash or unreadable history after that
//! commit therefore cannot lose a target: the post-commit side effects retry
//! whatever those entries still hold open.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};

use super::{TaskHistoryEntry, TaskRelation, TaskRelationType};
use crate::identity::is_valid_friction_id;

/// Task history event recording that resolving one named target is unfinished.
pub const RESOLVES_SIDE_EFFECT_PENDING_EVENT: &str = "resolves_side_effect_pending";
/// First-line prefix of every resolves side-effect history note.
pub const RESOLVES_TARGET_NOTE_PREFIX: &str = "target=";

/// State of a durable friction-resolution side effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolvesSideEffectState {
    /// The transition committed the obligation.
    Pending,
    /// An attempt failed.
    Failed,
    /// The obligation was settled.
    Recovered,
}

/// A side-effect obligation and optional already-redacted diagnostic detail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvesSideEffectRecord {
    /// The historical target, without further validation.
    pub target: String,
    /// The latest recorded state.
    pub state: ResolvesSideEffectState,
    /// Diagnostic text, if present.
    pub detail: Option<String>,
}

impl ResolvesSideEffectRecord {
    /// Decode the historical event and note, ignoring unrelated or malformed entries.
    pub fn from_history_entry(entry: &TaskHistoryEntry) -> Option<Self> {
        let state = match entry.event.as_str() {
            RESOLVES_SIDE_EFFECT_PENDING_EVENT => ResolvesSideEffectState::Pending,
            "resolves_side_effect_failed" => ResolvesSideEffectState::Failed,
            "resolves_side_effect_recovered" => ResolvesSideEffectState::Recovered,
            _ => return None,
        };
        let note = entry.note.as_deref()?;
        let target = note
            .lines()
            .next()?
            .strip_prefix(RESOLVES_TARGET_NOTE_PREFIX)?;
        let detail = note
            .split_once('\n')
            .and_then(|(_, rest)| rest.strip_prefix("reason: "));
        Some(Self {
            target: target.to_string(),
            state,
            detail: detail.map(str::to_string),
        })
    }

    /// Encode the legacy event and note without changing the history schema.
    pub fn into_history_entry(self, actor: &str, at: DateTime<Utc>) -> TaskHistoryEntry {
        let event = match self.state {
            ResolvesSideEffectState::Pending => RESOLVES_SIDE_EFFECT_PENDING_EVENT,
            ResolvesSideEffectState::Failed => "resolves_side_effect_failed",
            ResolvesSideEffectState::Recovered => "resolves_side_effect_recovered",
        };
        let mut note = format!("{RESOLVES_TARGET_NOTE_PREFIX}{}", self.target);
        if let Some(detail) = self.detail {
            note.push_str("\nreason: ");
            note.push_str(&detail);
        }
        TaskHistoryEntry::new(at, actor, event).with_note(Some(note))
    }
}

/// The friction targets of `relations`' `resolves` links, in relation order.
pub fn resolves_relation_targets(relations: &[TaskRelation]) -> impl Iterator<Item = &str> {
    relations
        .iter()
        .filter(|relation| relation.relation_type == TaskRelationType::Resolves)
        .map(|relation| relation.target.as_str())
        .filter(|target| is_valid_friction_id(target))
}

/// Every friction a task names: its `resolves` relations plus each valid
/// friction ID in `texts`, its description and criteria. Delimiters keep a
/// prefix of an unrelated identifier from being read as a target.
pub fn named_friction_targets<'a>(
    relations: &[TaskRelation],
    texts: impl IntoIterator<Item = &'a str>,
) -> BTreeSet<String> {
    let mut targets: BTreeSet<String> = resolves_relation_targets(relations)
        .map(str::to_string)
        .collect();
    for text in texts {
        for token in text.split(|ch: char| ch != '-' && ch != '_' && !ch.is_alphanumeric()) {
            if is_valid_friction_id(token) {
                targets.insert(token.to_string());
            }
        }
    }
    targets
}

/// The pending entries a transition into done commits for `targets`.
pub fn pending_resolves_entries(
    targets: &BTreeSet<String>,
    actor: &str,
    at: DateTime<Utc>,
) -> Vec<TaskHistoryEntry> {
    targets
        .iter()
        .map(|target| {
            ResolvesSideEffectRecord {
                target: target.clone(),
                state: ResolvesSideEffectState::Pending,
                detail: None,
            }
            .into_history_entry(actor, at)
        })
        .collect()
}
