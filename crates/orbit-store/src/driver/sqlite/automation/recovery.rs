//! Audited configuration, action, and history recovery for a delivery consumer.
//! The checkpoint and its immutable record commit together.

use super::{decode, encode};
use crate::Store;
use orbit_common::OrbitError;
use orbit_types::workflow::automation::recovery::{RecoveryRecord, ReissuedAction};
use orbit_types::workflow::automation::{AutomationState, BatchAttempt, BatchState};
use rusqlite::{TransactionBehavior, params};
use sha2::{Digest, Sha256};

pub(super) fn commit(
    store: &Store,
    previous: &AutomationState,
    next: &AutomationState,
    record: &RecoveryRecord,
) -> Result<bool, OrbitError> {
    validate(previous, next, record)?;

    let record_json = encode(record)?;
    let record_id = format!("{:x}", Sha256::digest(record_json.as_bytes()));

    store.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
        let conn = tx.connection();

        let changed = conn
            .execute(
                "UPDATE automation_consumers SET generation=?1,state_json=?2 WHERE consumer=?3 AND generation=?4 AND state_json=?5",
                params![
                    next.generation,
                    encode(next)?,
                    previous.consumer,
                    previous.generation,
                    encode(previous)?
                ],
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        if changed == 0 {
            return Ok(false);
        }

        conn.execute(
            "INSERT INTO automation_recoveries VALUES (?1,?2,?3,?4)",
            params![
                record_id,
                previous.consumer,
                record.at.to_rfc3339(),
                record_json
            ],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;

        Ok(true)
    })
}

pub(super) fn list(
    store: &Store,
    consumer: &str,
    limit: usize,
) -> Result<Vec<RecoveryRecord>, OrbitError> {
    store.with_read_connection(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT record_json FROM automation_recoveries WHERE consumer=?1 ORDER BY recorded_at DESC,rowid DESC LIMIT ?2",
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let rows = stmt
            .query_map(params![consumer, limit.min(100)], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        rows.map(|row| decode(&row.map_err(|e| OrbitError::Store(e.to_string()))?))
            .collect()
    })
}

/// A recovery may move the recorded configuration identity and replace one
/// settled attempt. Everything else — the cursors, the pending window, waived
/// and excluded landings, unresolved evidence and the frozen batch itself —
/// must arrive unchanged, so no recovery can skip or cover an obligation.
fn validate(
    previous: &AutomationState,
    next: &AutomationState,
    record: &RecoveryRecord,
) -> Result<(), OrbitError> {
    let invalid = || OrbitError::InvalidInput("invalid automation recovery transition".into());

    if previous.consumer != next.consumer
        || previous.consumer != record.consumer
        || previous.generation.checked_add(1) != Some(next.generation)
        || previous.members.is_some()
        || next.members.is_some()
        || previous.repository != next.repository
        || previous.branch != next.branch
        || previous.baseline != next.baseline
        || previous.covered != next.covered
        || previous.waived != next.waived
        || previous.excluded != next.excluded
    {
        return Err(invalid());
    }

    if record.reason.trim().is_empty() || record.by.trim().is_empty() {
        return Err(invalid());
    }

    // The record is the audit of this exact identity change, in both directions.
    if record.previous_epoch != previous.epoch
        || record.epoch != next.epoch
        || record.previous_trigger != previous.trigger
        || record.trigger != next.trigger
    {
        return Err(invalid());
    }

    if let Some(replay) = &record.replayed_history {
        return validate_history_replay(previous, next, record, replay);
    }

    if previous.observed != next.observed
        || previous.pending != next.pending
        || previous.pending_commits != next.pending_commits
        || previous.unresolved != next.unresolved
        || previous.associations != next.associations
    {
        return Err(invalid());
    }

    // Adoption is the only way the identity moves, and it has to move something.
    if record.adopted_settings {
        if next.epoch == previous.epoch && next.trigger == previous.trigger {
            return Err(invalid());
        }
    } else if next.epoch != previous.epoch || next.trigger != previous.trigger {
        return Err(invalid());
    }

    match (&previous.active, &next.active, &record.reissued) {
        (Some(settled), Some(claim), Some(reissued)) => {
            validate_reissue(settled, claim, reissued, record)
        }
        (settled, claim, None) if settled == claim => Ok(()),
        _ => Err(invalid()),
    }
}

fn validate_history_replay(
    previous: &AutomationState,
    next: &AutomationState,
    record: &RecoveryRecord,
    replay: &orbit_types::workflow::automation::recovery::HistoryReplayRecord,
) -> Result<(), OrbitError> {
    let invalid = || OrbitError::InvalidInput("invalid automation history replay".into());
    if record.adopted_settings
        || record.reissued.is_some()
        || previous.epoch != next.epoch
        || previous.trigger != next.trigger
        || previous.active != next.active
        || replay.captured_generation != previous.generation
        || replay.old_observed != previous.observed
        || replay.new_observed != next.observed
        || replay.unchanged_baseline != previous.baseline
        || replay.unchanged_covered != previous.covered
        || replay.mappings.is_empty()
        || replay.captured_head.commit.is_empty()
        || replay.mappings.iter().any(|mapping| {
            mapping.proof_digest.is_empty()
                || mapping.orphan.commit.is_empty()
                || mapping.canonical.commit.is_empty()
        })
        || replay
            .mappings
            .last()
            .is_none_or(|mapping| mapping.orphan != previous.observed)
    {
        return Err(invalid());
    }

    let old_keys = previous
        .pending
        .iter()
        .map(|delivery| delivery.key.as_str())
        .collect::<std::collections::HashSet<_>>();
    let new_keys = next
        .pending
        .iter()
        .map(|delivery| delivery.key.as_str())
        .collect::<std::collections::HashSet<_>>();
    if !old_keys.is_subset(&new_keys) || new_keys.len() != next.pending.len() {
        return Err(invalid());
    }
    for old in &previous.pending {
        let Some(new) = next
            .pending
            .iter()
            .find(|candidate| candidate.key == old.key)
        else {
            return Err(invalid());
        };
        if old.repository != new.repository
            || old.branch != new.branch
            || old.task_ids != new.task_ids
            || old.evidence_reference != new.evidence_reference
            || old.landed_at != new.landed_at
        {
            return Err(invalid());
        }
    }
    let added = new_keys
        .difference(&old_keys)
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let recorded = replay
        .added_obligations
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let mut expected_unresolved = previous.unresolved.clone();
    for mapping in &replay.mappings {
        let Some(reason) = expected_unresolved.remove(&mapping.orphan.commit) else {
            continue;
        };
        if expected_unresolved
            .insert(mapping.canonical.commit.clone(), reason)
            .is_some()
        {
            return Err(invalid());
        }
    }

    let mapped_orphans = replay
        .mappings
        .iter()
        .map(|mapping| mapping.orphan.commit.as_str())
        .collect::<std::collections::HashSet<_>>();
    let retained_commits = previous
        .pending_commits
        .iter()
        .filter(|commit| !mapped_orphans.contains(commit.as_str()));

    if added != recorded
        || next.unresolved != expected_unresolved
        || !retained_commits
            .into_iter()
            .all(|commit| next.pending_commits.contains(commit))
        || next.pending_commits.len() > 5000
        || next.pending.len() > 1000
        || next
            .pending_commits
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != next.pending_commits.len()
        || next.pending.iter().any(|delivery| {
            delivery.commits.is_empty()
                || !delivery
                    .commits
                    .iter()
                    .all(|commit| next.pending_commits.contains(commit))
        })
        || next
            .unresolved
            .keys()
            .any(|commit| !next.pending_commits.contains(commit))
        || next
            .associations
            .keys()
            .any(|commit| !next.pending_commits.contains(commit))
    {
        return Err(invalid());
    }

    Ok(())
}

/// The reissued attempt keeps the frozen batch and its obligations, takes the
/// next attempt number, and carries the operator authorization that grants it.
fn validate_reissue(
    settled: &BatchAttempt,
    claim: &BatchAttempt,
    reissued: &ReissuedAction,
    record: &RecoveryRecord,
) -> Result<(), OrbitError> {
    let invalid = || OrbitError::InvalidInput("invalid automation recovery transition".into());

    if settled.batch != claim.batch
        || settled.input_digest != claim.input_digest
        || !matches!(settled.state, BatchState::Failed | BatchState::Exhausted)
        || claim.state != BatchState::Claimed
        || claim.action_id.is_some()
        || claim.reason.is_some()
        || claim.retry_after.is_some()
        || claim.attempt != settled.attempt.saturating_add(1)
        || claim.action_key != format!("automation:{}:{}", claim.batch.id, claim.attempt)
    {
        return Err(invalid());
    }

    let Some(authorization) = &claim.reissue else {
        return Err(invalid());
    };

    if authorization != &reissued.authorization
        || authorization.from_action_id != settled.action_id
        || authorization.by != record.by
        || authorization.reason != record.reason
        || authorization.at != record.at
        || authorization.retry_until <= record.at
    {
        return Err(invalid());
    }

    if reissued.batch_id != settled.batch.id
        || reissued.from_action_id != settled.action_id
        || reissued.from_attempt != settled.attempt
        || reissued.from_state != settled.state
        || reissued.from_reason != settled.reason
        || reissued.attempt != claim.attempt
    {
        return Err(invalid());
    }

    Ok(())
}
