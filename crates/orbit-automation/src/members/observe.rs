//! Fold observation pages into the bounded member working set.

use super::{MemberHost, MemberPage};
use crate::AutomationError;
use orbit_types::workflow::automation::members::*;
use std::collections::BTreeSet;

pub(super) use orbit_types::workflow::automation::members::MEMBER_CAPACITY as CAPACITY;

/// Fold one page into the working set without exceeding [`CAPACITY`], and
/// return how many of its entries must wait for room. Entries already
/// retained always update; at capacity, a member's fresh fingerprint takes
/// the place of its superseded assessment, whose receipt stays durable.
pub(super) fn absorb(host: &dyn MemberHost, members: &mut MemberState, page: &MemberPage) -> usize {
    let mut deferred = 0;

    for (key, reason) in &page.withheld {
        if members.pending.remove(key).is_none()
            && !members.withheld.contains_key(key)
            && members.retained() >= CAPACITY
        {
            deferred += 1;
            continue;
        }
        members.withheld.insert(key.clone(), reason.clone());
    }

    for member in &page.candidates {
        let mut member = member.clone();
        members.withheld.remove(&member.key);
        for task_id in &member.task_ids {
            members.withheld.remove(task_id);
        }

        // Already assessed at exactly this fingerprint, or under an earlier
        // contract whose assessment still holds: nothing left to apply.
        if members.assessed.get(&member.key).is_some_and(|assessed| {
            assessed.resulting_fingerprint == member.fingerprint
                || host.carries_forward(&member, assessed)
        }) {
            members.pending.remove(&member.key);
            continue;
        }

        // Re-seeing a member preserves how long it has waited, and an unchanged
        // fingerprint also preserves when it last changed.
        if let Some(old) = members.pending.get(&member.key) {
            member.first_seen = old.first_seen;
            if old.fingerprint == member.fingerprint {
                member.changed_at = old.changed_at;
            }
        } else if members.retained() >= CAPACITY && members.assessed.remove(&member.key).is_none() {
            deferred += 1;
            continue;
        }

        members.pending.insert(member.key.clone(), member);
    }

    deferred
}

/// Every key `page` observes: each candidate's member key and task ids, and
/// each key it withheld.
pub(super) fn observed(page: &MemberPage) -> BTreeSet<String> {
    page.candidates
        .iter()
        .flat_map(|member| std::iter::once(&member.key).chain(&member.task_ids))
        .chain(page.withheld.keys())
        .cloned()
        .collect()
}

/// Drop each failed record whose member is now pending at a fresh
/// fingerprint: it no longer withholds anything, and the member's next
/// failure records itself afresh. The in-flight attempt's members and the
/// `kept` keys keep theirs.
pub(super) fn retire_superseded(members: &mut MemberState, kept: &BTreeSet<String>) {
    let pending = &members.pending;
    let active = &members.active;
    members.failed.retain(|key, failed| {
        kept.contains(key)
            || active
                .as_ref()
                .is_some_and(|active| active.member_for(key).is_some())
            || !pending.get(key).is_some_and(|pending| {
                failed
                    .member_for(key)
                    .is_some_and(|retired| retired.fingerprint != pending.fingerprint)
            })
    });
}

/// Retire the working state and failed records of members the source no
/// longer observes. Observation is paged, so absence from one page proves
/// nothing: the host answers for every retained key by identity instead.
/// Receipts stay durable, and a member that returns is assessed afresh. The
/// in-flight attempt's members and the `kept` keys stay.
pub(super) fn retire_unobserved(
    host: &dyn MemberHost,
    members: &mut MemberState,
    mut kept: BTreeSet<String>,
) -> Result<(), AutomationError> {
    if let Some(active) = &members.active {
        kept.extend(active.members().iter().map(|member| member.key.clone()));
    }

    let keys = members
        .pending
        .keys()
        .chain(members.assessed.keys())
        .chain(members.withheld.keys())
        .chain(members.failed.keys())
        .filter(|key| !kept.contains(*key))
        .cloned()
        .collect::<BTreeSet<_>>();
    if keys.is_empty() {
        return Ok(());
    }

    let observable = host.observable(&keys)?;
    for key in keys.difference(&observable) {
        members.pending.remove(key);
        members.assessed.remove(key);
        members.withheld.remove(key);
        members.failed.remove(key);
    }

    Ok(())
}
