//! Monotonic bounded observation; no observation certifies examination.
use crate::AutomationError;
use orbit_types::workflow::automation::{AutomationState, SourcePage};
pub(super) fn apply(
    state: &AutomationState,
    page: SourcePage,
) -> Result<AutomationState, AutomationError> {
    let invalid = || AutomationError::Deferred("source_page_invalid".into());
    if page.from != state.observed
        || page.commits.len() > 200
        || page.deliveries.len() > 50
        || page
            .commits
            .last()
            .is_some_and(|sha| sha != &page.through.commit)
        || (page.commits.is_empty() && page.from != page.through)
    {
        return Err(invalid());
    }
    let mut next = state.clone();
    for sha in &page.commits {
        if next.pending_commits.contains(sha) {
            return Err(invalid());
        }
        next.pending_commits.push(sha.clone());
    }
    if next.pending_commits.len() > 5000 {
        return Err(AutomationError::Deferred("source_backpressure".into()));
    }
    for (sha, association) in page.associations {
        if next.pending_commits.contains(&sha) {
            next.associations.insert(sha, association);
        }
    }
    for d in page.deliveries {
        if d.key.is_empty()
            || d.repository != state.repository
            || d.branch != state.branch
            || d.commits.is_empty()
            || d.evidence_digest.is_empty()
            || d.evidence_reference.is_empty()
        {
            return Err(invalid());
        }
        if state.waived.iter().any(|old| old.key == d.key) {
            continue;
        }
        if d.before.tree == d.after.tree {
            continue;
        }
        // Late provider evidence only creates debt for still-unexamined content.
        if !d
            .commits
            .iter()
            .any(|sha| next.pending_commits.contains(sha))
        {
            continue;
        }
        if !d
            .commits
            .iter()
            .all(|sha| next.pending_commits.contains(sha))
        {
            return Err(AutomationError::Deferred(
                "delivery_crosses_boundary".into(),
            ));
        }
        if let Some(old) = next.pending.iter().find(|old| old.key == d.key) {
            if old != &d {
                return Err(AutomationError::Deferred(
                    "delivery_evidence_changed".into(),
                ));
            }
            continue;
        }
        if next
            .pending
            .iter()
            .any(|old| old.commits.iter().any(|sha| d.commits.contains(sha)))
        {
            return Err(AutomationError::Deferred(
                "delivery_grouping_ambiguous".into(),
            ));
        }
        for sha in &d.commits {
            next.unresolved.remove(sha);
        }
        next.pending.push(d);
    }
    for (sha, reason) in page.unresolved {
        if next.pending_commits.contains(&sha)
            && !next.pending.iter().any(|d| d.commits.contains(&sha))
        {
            next.unresolved.insert(sha, reason);
        }
    }
    next.pending.sort_by_key(|d| {
        next.pending_commits
            .iter()
            .position(|sha| sha == &d.after.commit)
    });
    next.observed = page.through;
    if next.pending.len() > 1000 {
        return Err(AutomationError::Deferred("source_backpressure".into()));
    }
    Ok(next)
}
