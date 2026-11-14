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
            TaskHistoryEntry::new(at, actor, RESOLVES_SIDE_EFFECT_PENDING_EVENT)
                .with_note(Some(format!("{RESOLVES_TARGET_NOTE_PREFIX}{target}")))
        })
        .collect()
}
