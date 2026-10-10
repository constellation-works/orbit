//! State-member invariants use the same transaction and receipt tables.

use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::workflow::automation::{
    AcceptedCoverage, AutomationState,
    members::{MEMBER_CAPACITY, MemberAttempt, MemberBatchEvidence, MemberState, StateTriggerKind},
};
use orbit_types::workflow::{JobRunState, PipelineState};
use rusqlite::{Connection, params};
use std::collections::BTreeSet;

use super::codec::{decode, encode};

pub(super) fn validate(
    previous: &AutomationState,
    next: &AutomationState,
    receipt: Option<&AcceptedCoverage>,
) -> Result<(), OrbitError> {
    let invalid = || OrbitError::InvalidInput("invalid member checkpoint transition".into());
    let old = previous.members.as_ref().ok_or_else(invalid)?;
    let new = next.members.as_ref().ok_or_else(invalid)?;

    // A member checkpoint touches member state only; the delivery projection and
    // its cursors must come through untouched.
    if previous.active.is_some()
        || next.active.is_some()
        || previous.covered != next.covered
        || previous.observed != next.observed
        || previous.pending != next.pending
        || previous.pending_commits != next.pending_commits
        || previous.waived != next.waived
        || new.retained() > MEMBER_CAPACITY
        || new.failed.len() > MEMBER_CAPACITY
    {
        return Err(invalid());
    }

    // An in-flight attempt keeps its identity and may only advance by one retry;
    // before admission it may shrink to the members still admissible.
    if let Some(active) = &old.active {
        if let Some(updated) = &new.active {
            let batch_kept = active.members() == updated.members();
            let batch_shrunk = active.action_id.is_none()
                && updated.attempt == active.attempt
                && updated.batch_is_consistent()
                && updated.members().len() < active.members().len()
                && updated.members().iter().all(|member| {
                    active
                        .member_for(&member.key)
                        .is_some_and(|carried| carried == member)
                });
            if active.consumer != updated.consumer
                || active.kind != updated.kind
                || active.id != updated.id
                || !(batch_kept || batch_shrunk)
                || active.max_attempts != updated.max_attempts
                || active.deadline != updated.deadline
                || updated.attempt < active.attempt
                || updated.attempt > active.attempt + 1
                || (updated.attempt == active.attempt
                    && (updated.action_key != active.action_key
                        || (active.action_id.is_some() && updated.action_id != active.action_id)))
                || (updated.attempt > active.attempt
                    && (updated.action_id.is_some()
                        || updated.action_key
                            != format!("automation:{}:{}", active.id, updated.attempt)))
                || updated.attempt > active.max_attempts
            {
                return Err(invalid());
            }
        } else if receipt.is_none() && !every_member_settled(active, new, &BTreeSet::new()) {
            return Err(invalid());
        }
    }

    // A newly claimed attempt starts at attempt 1, unadmitted, over pending members.
    if old.active.is_none()
        && let Some(active) = &new.active
        && (active.consumer != previous.consumer
            || active.attempt != 1
            || active.max_attempts == 0
            || active.max_attempts > 6
            || !active.batch_is_consistent()
            || !active
                .members()
                .iter()
                .all(|member| new.pending.values().any(|pending| pending == member))
            || active.action_id.is_some()
            || active.exhausted
            || active.deadline <= active.retry_after)
    {
        return Err(invalid());
    }

    // Members a receipt certifies; the attempt's other members are the only
    // ones a new failed record may be written for.
    let mut certified = BTreeSet::new();
    if let Some(receipt) = receipt {
        let active = old.active.as_ref().ok_or_else(invalid)?;
        let evidence: MemberBatchEvidence =
            serde_json::from_slice(&receipt.evidence).map_err(|_| invalid())?;
        let bytes = active.identity_bytes().map_err(|_| invalid())?;

        if receipt.batch_id != active.id
            || active.action_id.as_ref() != Some(&receipt.action_id)
            || receipt.input_digest != sha256_hex(&bytes)
            || receipt.evidence_digest != sha256_hex(&receipt.evidence)
            || evidence.action_id != receipt.action_id
            || evidence.attempt_id != active.id
            || evidence.applied.is_empty()
            || new.active.is_some()
            || receipt.submitted_by != "system"
        {
            return Err(invalid());
        }

        // The receipt adds exactly one assessment per member it applied, each
        // certifying that member's own input, and retires every other member.
        let mut expected_assessments = old.assessed.clone();
        let mut applied = BTreeSet::new();
        for applied_member in &evidence.applied {
            let member = active
                .member_for(&applied_member.member_key)
                .ok_or_else(invalid)?;
            let assessment = new
                .assessed
                .get(&applied_member.member_key)
                .ok_or_else(invalid)?;
            if applied_member.action_id != receipt.action_id
                || applied_member.attempt_id != active.id
                || applied_member.input_fingerprint != member.fingerprint
                || applied_member.resulting_fingerprint.is_empty()
                || applied_member.result.is_null()
                || assessment.input_fingerprint != applied_member.input_fingerprint
                || assessment.resulting_fingerprint != applied_member.resulting_fingerprint
                || assessment.ready != applied_member.ready
                || assessment.receipt_id != active.id
                || !applied.insert(applied_member.member_key.clone())
            {
                return Err(invalid());
            }
            expected_assessments.insert(applied_member.member_key.clone(), assessment.clone());
        }

        if expected_assessments != new.assessed || !every_member_settled(active, new, &applied) {
            return Err(invalid());
        }
        certified = applied;
    } else if new
        .assessed
        .iter()
        .any(|(key, assessment)| old.assessed.get(key) != Some(assessment))
    {
        // Without a receipt an assessment may only be retired from working
        // state, never added or rewritten; its receipt stays durable.
        return Err(invalid());
    }

    let retiring = old.active.as_ref().filter(|_| new.active.is_none());
    if !failed_records_valid(old, new, retiring, &certified) {
        return Err(invalid());
    }

    Ok(())
}

/// Failed records are append-only evidence [ORB-14177]. One may be written
/// only as the exact [`MemberAttempt::failure_record`] of the attempt this
/// checkpoint retires, for a member it carried and did not certify; a stored
/// record otherwise stays, is compacted to its own member's failure record,
/// or leaves once it suppresses nothing: its member is neither in flight nor
/// pending at the fingerprint it failed at.
fn failed_records_valid(
    old: &MemberState,
    new: &MemberState,
    retiring: Option<&MemberAttempt>,
    certified: &BTreeSet<String>,
) -> bool {
    let retired_record = |key: &str| {
        retiring
            .filter(|_| !certified.contains(key))
            .and_then(|active| active.failure_record(key))
    };

    let kept = old.failed.iter().all(|(key, failed)| {
        let Some(next) = new.failed.get(key) else {
            return new
                .active
                .as_ref()
                .is_none_or(|active| active.member_for(key).is_none())
                && !failed.member_for(key).is_some_and(|retired| {
                    new.pending
                        .get(key)
                        .is_some_and(|pending| pending.fingerprint == retired.fingerprint)
                });
        };
        next == failed
            || (failed.exhausted && failed.failure_record(key).as_ref() == Some(next))
            || retired_record(key).as_ref() == Some(next)
    });

    kept && new.failed.iter().all(|(key, next)| {
        old.failed.contains_key(key) || retired_record(key).as_ref() == Some(next)
    })
}

/// Whether every member of `active` outside `except` now holds this very
/// attempt's failure record, or was released for a fresh claim: the only ways
/// an attempt clears the slot without certifying a member. A released member
/// is no longer pending at the source the attempt froze [ORB-14476], so no
/// later claim can replay that source, and it needs no failure record.
fn every_member_settled(
    active: &MemberAttempt,
    new: &MemberState,
    except: &BTreeSet<String>,
) -> bool {
    active
        .members()
        .iter()
        .filter(|member| !except.contains(&member.key))
        .all(|member| {
            new.failed.get(&member.key) == active.failure_record(&member.key).as_ref()
                || new
                    .pending
                    .get(&member.key)
                    .is_none_or(|pending| pending.source != member.source)
        })
}

/// One-time repair [ORB-14476]. Before a source move could supersede an
/// attempt, a task-pilot claim whose branch moved under its material failed,
/// retried against the same frozen source, failed again and was retired at
/// the task's fingerprint, so the task was never piloted again until someone
/// edited it. Persisted state does not record why an attempt failed, so this
/// releases every member shelved at a source the branch has since left: its
/// exhausted failure record is at the fingerprint it is still pending at, and
/// it is pending at another source. A member that failed for another reason
/// costs at most one more pilot attempt. Each repaired consumer advances its
/// generation, so a writer holding the old snapshot is refused.
pub(super) fn release_stale_source_failures(conn: &Connection) -> Result<(), OrbitError> {
    release_failures(conn, |members, key, failed| {
        Ok(shelved_by_source(members, key, failed))
    })
}

/// The global step index `apply` held in `task_pilot_pipeline` until a step
/// was inserted before it, and where the member host kept reading it.
const MISREAD_APPLY_INDEX: u32 = 2;

/// One-time repair [ORB-15197]. The member host read `task_pilot_pipeline`
/// steps by position. Once a step was inserted before `apply`, it took the
/// `pilots` fan-in output at the old apply position for the apply record,
/// found no member evidence there and recorded every member failed, often
/// while the real apply was still running, so no assessment was certified and
/// a run that needed a repair apply lost its claim. Each such record names a
/// task-pilot run whose checkpoint at that position is not its `apply`
/// checkpoint; this releases those records so the members are observed afresh
/// and the next pilot certifies them. A record whose run cannot be read, or
/// whose run recorded `apply` at that position, stays. Each repaired consumer
/// advances its generation, so a writer holding the old snapshot is refused.
pub(super) fn release_misread_pilot_failures(conn: &Connection) -> Result<(), OrbitError> {
    let states_exist = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='job_run_states')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    if !states_exist {
        return Ok(());
    }
    release_failures(conn, |members, key, failed| {
        if failed.kind != StateTriggerKind::PreparationEligible
            || members
                .active
                .as_ref()
                .is_some_and(|active| active.member_for(key).is_some())
        {
            return Ok(false);
        }
        let Some(run_id) = failed.action_id.as_deref() else {
            return Ok(false);
        };
        misread_apply(conn, run_id)
    })
}

/// Whether every stored state of the task-pilot run `run_id` holds a
/// successful checkpoint at [`MISREAD_APPLY_INDEX`] that is not its `apply`
/// output. An unreadable state proves nothing and answers `false`.
fn misread_apply(conn: &Connection, run_id: &str) -> Result<bool, OrbitError> {
    let states = {
        let mut statement = conn
            .prepare(
                "SELECT s.pipeline_state_json FROM job_run_states s \
                 JOIN job_runs r ON r.workspace_id = s.workspace_id AND r.run_id = s.run_id \
                 WHERE s.run_id = ?1 AND r.job_id = 'task_pilot_pipeline'",
            )
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        statement
            .query_map([run_id], |row| row.get::<_, String>(0))
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(|error| OrbitError::Store(error.to_string()))?
    };
    Ok(!states.is_empty()
        && states.iter().all(|raw| {
            decode::<PipelineState>(raw).is_ok_and(|state| {
                state.step_states.get(&MISREAD_APPLY_INDEX) == Some(&JobRunState::Success)
                    && state
                        .step_output(MISREAD_APPLY_INDEX)
                        .is_some_and(|output| state.pipeline.get("apply") != Some(output))
            })
        }))
}

/// Remove each failed record `release` selects, with the withheld reason it
/// left, from every consumer this binary can read.
fn release_failures(
    conn: &Connection,
    release: impl Fn(&MemberState, &str, &MemberAttempt) -> Result<bool, OrbitError>,
) -> Result<(), OrbitError> {
    let rows = {
        let mut statement = conn
            .prepare("SELECT consumer, generation, state_json FROM automation_consumers")
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(|error| OrbitError::Store(error.to_string()))?
    };

    for (consumer, generation, raw) in rows {
        // A record this binary cannot read is left for its own reader.
        let Ok(mut state) = decode::<AutomationState>(&raw) else {
            continue;
        };
        let Some(members) = state.members.as_mut() else {
            continue;
        };
        let mut released = Vec::new();
        for (key, failed) in &members.failed {
            if release(members, key, failed)? {
                released.push(key.clone());
            }
        }
        if released.is_empty() {
            continue;
        }
        for key in &released {
            members.failed.remove(key);
            members.withheld.remove(key);
        }
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or_else(|| OrbitError::Store("automation generation overflow".into()))?;
        conn.execute(
            "UPDATE automation_consumers SET generation=?1, state_json=?2 WHERE consumer=?3 AND generation=?4",
            params![state.generation, encode(&state)?, consumer, generation],
        )
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    }
    Ok(())
}

fn shelved_by_source(members: &MemberState, key: &str, failed: &MemberAttempt) -> bool {
    failed.kind == StateTriggerKind::PreparationEligible
        && failed.exhausted
        && members
            .active
            .as_ref()
            .is_none_or(|active| active.member_for(key).is_none())
        && failed.member_for(key).is_some_and(|retired| {
            members.pending.get(key).is_some_and(|pending| {
                pending.fingerprint == retired.fingerprint && pending.source != retired.source
            })
        })
}
