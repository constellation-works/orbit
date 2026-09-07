//! Monotonic bounded observation; no observation certifies examination.

use crate::AutomationError;
use orbit_types::workflow::automation::{
    AutomationState, CoverageClass, ExcludedDelivery, SourcePage,
};

pub(super) fn apply(
    state: &AutomationState,
    page: SourcePage,
    coverage: CoverageClass,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<AutomationState, AutomationError> {
    let invalid = || AutomationError::Deferred("source_page_invalid".into());

    // A page must resume exactly where the cursor stands, stay within its bounds,
    // and end on the revision it claims to reach.
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

    for delivery in page.deliveries {
        if delivery.key.is_empty()
            || delivery.repository != state.repository
            || delivery.branch != state.branch
            || delivery.commits.is_empty()
            || delivery.evidence_digest.is_empty()
            || delivery.evidence_reference.is_empty()
        {
            return Err(invalid());
        }

        if state.waived.iter().any(|old| old.key == delivery.key)
            || state
                .excluded
                .iter()
                .any(|old| old.delivery.key == delivery.key)
        {
            continue;
        }

        if delivery.before.tree == delivery.after.tree {
            continue;
        }

        // Late provider evidence only creates debt for still-unexamined content.
        if !delivery
            .commits
            .iter()
            .any(|sha| next.pending_commits.contains(sha))
        {
            continue;
        }

        if !delivery
            .commits
            .iter()
            .all(|sha| next.pending_commits.contains(sha))
        {
            return Err(AutomationError::Deferred(
                "delivery_crosses_boundary".into(),
            ));
        }

        // A repeated key is only tolerable when the provider restated identical evidence.
        if let Some(old) = next.pending.iter().find(|old| old.key == delivery.key) {
            if old != &delivery {
                return Err(AutomationError::Deferred(
                    "delivery_evidence_changed".into(),
                ));
            }
            continue;
        }

        if next
            .pending
            .iter()
            .any(|old| old.commits.iter().any(|sha| delivery.commits.contains(sha)))
        {
            return Err(AutomationError::Deferred(
                "delivery_grouping_ambiguous".into(),
            ));
        }

        for sha in &delivery.commits {
            next.unresolved.remove(sha);
        }

        // Proven before-PR coverage is an exclusion only for a review consumer:
        // QA still owes every landing its own integrated examination.
        if coverage == CoverageClass::LandedCodeReviewV1
            && let Some(exclusion) = page.exclusions.get(&delivery.key)
        {
            next.excluded.push(ExcludedDelivery {
                delivery,
                exclusion: exclusion.clone(),
                decided_at: now,
            });
            continue;
        }

        next.pending.push(delivery);
    }

    // Only commits no delivery accounted for stay recorded as unresolved.
    for (sha, reason) in page.unresolved {
        if next.pending_commits.contains(&sha)
            && !next.pending.iter().any(|d| d.commits.contains(&sha))
            && !next
                .excluded
                .iter()
                .any(|excluded| excluded.delivery.commits.contains(&sha))
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
