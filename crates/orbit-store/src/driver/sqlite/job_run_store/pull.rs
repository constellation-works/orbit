//! Durable caller admissions share the job database writer transaction. Receipt
//! replay never creates a second leaf, including after the first leaf terminates.
use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::workflow::{JobRun, JobRunState, PipelineState, RunIdRole};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::queries::{
    get_job_run_for_workspace_conn, next_run_id_conn, upsert_job_run_for_workspace_conn,
};
use crate::Store;
use crate::contracts::{
    AdmissionRequest, ClaimMutation, LocalPullAdmission, LocalPullMutation, LocalPullPhase,
    PullDestination,
};
use crate::driver::sqlite::migration::FeatureMigration;

fn initialize(store: &Store) -> Result<(), OrbitError> {
    store.apply_feature_migrations("local_pull", &[FeatureMigration::new(1, "pending_requests_and_unique_leaves", |conn| {
        conn.execute_batch("CREATE TABLE local_pull_admissions (
            workspace_id TEXT NOT NULL, owner_machine TEXT NOT NULL,
            owner_workspace TEXT NOT NULL, execution_machine TEXT NOT NULL,
            request_id TEXT NOT NULL, claim_id TEXT, leaf_run_id TEXT, record_json TEXT NOT NULL,
            PRIMARY KEY(workspace_id, owner_machine, owner_workspace, execution_machine, request_id),
            UNIQUE(workspace_id, owner_machine, owner_workspace, claim_id),
            UNIQUE(workspace_id, leaf_run_id));")
            .map_err(db_error)
    })])
}

fn db_error(error: impl std::fmt::Display) -> OrbitError {
    OrbitError::Store(error.to_string())
}
fn invalid(message: &str) -> OrbitError {
    OrbitError::JobValidation(message.into())
}
fn decode(raw: String) -> Result<LocalPullAdmission, OrbitError> {
    serde_json::from_str(&raw).map_err(db_error)
}
fn records(conn: &Connection, workspace: &str) -> Result<Vec<LocalPullAdmission>, OrbitError> {
    let mut stmt = conn
        .prepare(
            "SELECT record_json FROM local_pull_admissions WHERE workspace_id=?1 ORDER BY rowid",
        )
        .map_err(db_error)?;
    let raw = stmt
        .query_map([workspace], |r| r.get::<_, String>(0))
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    raw.into_iter().map(decode).collect()
}
fn read(
    conn: &Connection,
    workspace: &str,
    destination: &PullDestination,
    request_id: &str,
) -> Result<Option<LocalPullAdmission>, OrbitError> {
    conn.query_row("SELECT record_json FROM local_pull_admissions WHERE workspace_id=?1 AND owner_machine=?2 AND owner_workspace=?3 AND execution_machine=?4 AND request_id=?5",
        params![workspace, destination.owner_machine_id, destination.owner_workspace_id, destination.execution_machine_id, request_id], |r| r.get::<_, String>(0))
        .optional().map_err(db_error)?.map(decode).transpose()
}
fn write(
    conn: &Connection,
    workspace: &str,
    record: &LocalPullAdmission,
) -> Result<(), OrbitError> {
    let claim = record
        .receipt
        .as_ref()
        .and_then(|r| r.claim.as_ref())
        .map(|c| c.claim_id.as_str());
    conn.execute("INSERT INTO local_pull_admissions VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
        ON CONFLICT(workspace_id,owner_machine,owner_workspace,execution_machine,request_id)
        DO UPDATE SET claim_id=excluded.claim_id, leaf_run_id=excluded.leaf_run_id, record_json=excluded.record_json",
        params![workspace, record.destination.owner_machine_id, record.destination.owner_workspace_id,
            record.destination.execution_machine_id, record.request.request_id, claim, record.leaf_run_id,
            serde_json::to_string(record).map_err(db_error)?]).map_err(db_error)?;
    Ok(())
}

/// Existing generic workers/resume reads must neither create a feature schema
/// nor scan all historical receipts merely to discover that a run is unclaimed.
pub(super) fn for_run(
    store: &Store,
    workspace: &str,
    run_id: &str,
) -> Result<Option<LocalPullAdmission>, OrbitError> {
    store.with_read_connection(|conn| {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='local_pull_admissions')",
            [], |row| row.get(0)).map_err(db_error)?;
        if !exists { return Ok(None); }
        conn.query_row("SELECT record_json FROM local_pull_admissions WHERE workspace_id=?1 AND leaf_run_id=?2",
            params![workspace, run_id], |row| row.get::<_, String>(0))
            .optional().map_err(db_error)?.map(decode).transpose()
    })
}

pub(super) fn list(store: &Store, workspace: &str) -> Result<Vec<LocalPullAdmission>, OrbitError> {
    initialize(store)?;
    store.with_read_connection(|conn| records(conn, workspace))
}

/// Count legacy wrappers once, replacing them with their actual PR/local
/// descendants. Bound queued leaves and admissions lacking an active leaf each
/// consume one slot. A terminal leaf retains its slot until settlement.
fn occupancy(
    conn: &Connection,
    workspace: &str,
) -> Result<(usize, BTreeMap<String, usize>), OrbitError> {
    let mut stmt = conn.prepare("SELECT run_id,job_id,pipeline_state_json FROM job_runs WHERE workspace_id=?1 AND state IN ('pending','running','retrying')").map_err(db_error)?;
    let active = stmt
        .query_map([workspace], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    let mut slots = BTreeSet::new();
    let mut pipelines = BTreeMap::new();
    let leaves: BTreeSet<_> = active
        .iter()
        .filter(|(_, job, _)| is_leaf_pipeline(job))
        .map(|(id, _, _)| id.clone())
        .collect();
    slots.extend(leaves.iter().cloned());
    for (id, job, state) in &active {
        if is_leaf_pipeline(job) {
            *pipelines.entry(job.clone()).or_insert(0) += 1;
        } else if job == "task_auto_pipeline" {
            let mut frontier = vec![state.clone()];
            let mut seen = BTreeSet::new();
            for _ in 0..64 {
                let mut next = Vec::new();
                for raw in frontier.into_iter().flatten() {
                    let state: PipelineState = serde_json::from_str(&raw).map_err(db_error)?;
                    for child in state.child_dispatches {
                        if seen.insert(child.child_run_id.clone()) {
                            let raw: Option<String> = conn.query_row(
                                "SELECT pipeline_state_json FROM job_runs WHERE workspace_id=?1 AND run_id=?2",
                                params![workspace, child.child_run_id], |r| r.get(0))
                                .optional().map_err(db_error)?.flatten();
                            next.push(raw);
                        }
                    }
                }
                if next.is_empty() {
                    break;
                }
                frontier = next;
            }
            if seen.is_disjoint(&leaves) {
                slots.insert(id.clone());
            }
        }
    }
    let mut occupied = slots.len();
    for record in records(conn, workspace)? {
        if matches!(record.phase, LocalPullPhase::Idle | LocalPullPhase::Settled) {
            continue;
        }
        if record
            .leaf_run_id
            .as_ref()
            .is_none_or(|id| !slots.contains(id))
        {
            occupied += 1;
        }
        if record
            .leaf_run_id
            .as_ref()
            .is_none_or(|id| !active.iter().any(|(active_id, _, _)| active_id == id))
        {
            *pipelines
                .entry(pipeline(&record.request)?.to_string())
                .or_insert(0) += 1;
        }
    }
    Ok((occupied, pipelines))
}
/// The leaf definitions a claim may select. They are the handoff-only claimed
/// variants, never the merge-capable legacy pipelines: a pulled claim settles
/// through the owner, so a leaf that could merge or complete on its own would
/// bypass the lifecycle the claim exists to enforce.
fn pipeline(request: &AdmissionRequest) -> Result<&'static str, OrbitError> {
    match request.ship.mode.as_str() {
        "pr" => Ok(CLAIMED_PR_PIPELINE),
        "local" => Ok(CLAIMED_LOCAL_PIPELINE),
        _ => Err(invalid("unsupported pulled leaf mode")),
    }
}

pub(crate) const CLAIMED_PR_PIPELINE: &str = "task_claimed_pr_pipeline";
pub(crate) const CLAIMED_LOCAL_PIPELINE: &str = "task_claimed_local_pipeline";

/// Every leaf definition that occupies one drain slot: the legacy pair a
/// non-pulled drain still dispatches, and the claimed pair a pulled one does.
fn is_leaf_pipeline(job: &str) -> bool {
    matches!(
        job,
        "task_pr_pipeline" | "task_local_pipeline" | CLAIMED_PR_PIPELINE | CLAIMED_LOCAL_PIPELINE
    )
}

pub(super) fn allocate(
    store: &Store,
    workspace: &str,
    destination: &PullDestination,
    request: &AdmissionRequest,
    ceiling: usize,
) -> Result<Option<LocalPullAdmission>, OrbitError> {
    initialize(store)?;
    if [
        &destination.owner_machine_id,
        &destination.owner_workspace_id,
        &destination.execution_machine_id,
        &destination.selector,
        &request.request_id,
    ]
    .iter()
    .any(|s| s.trim().is_empty())
    {
        return Err(invalid(
            "pull destination and request identity are required",
        ));
    }
    if request.ship.mode == "local"
        && destination.owner_machine_id != destination.execution_machine_id
    {
        return Err(invalid("followers cannot execute owner-local leaves"));
    }
    if request.ship.review_policy != "none" || request.caller_review_policy != "none" {
        return Err(invalid("pulled leaves require review policy none"));
    }
    store.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
        let conn = tx.connection();
        if let Some(old) = read(conn, workspace, destination, &request.request_id)? {
            if old.request != *request || old.destination != *destination {
                return Err(invalid("pull request identity reused with changed input"));
            }
            return Ok(Some(old));
        }
        let parent = get_job_run_for_workspace_conn(conn, workspace, &request.run_context.run_id)?
            .ok_or_else(|| invalid("pull drain run missing"))?;
        if parent.state.is_terminal() {
            return Ok(None);
        }
        let raw: Option<String> = conn
            .query_row(
                "SELECT pipeline_state_json FROM job_runs WHERE workspace_id=?1 AND run_id=?2",
                params![workspace, parent.run_id],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        let state: PipelineState =
            serde_json::from_str(&raw.ok_or_else(|| invalid("pull drain state missing"))?)
                .map_err(db_error)?;
        if state.admissions_stopped() {
            return Ok(None);
        }
        let ceiling = state
            .effective_max_active_leaf_runs(u32::try_from(ceiling).unwrap_or(u32::MAX))
            as usize;
        let (occupied, pipelines) = occupancy(conn, workspace)?;
        if occupied >= ceiling || pipelines.get(pipeline(request)?).copied().unwrap_or(0) >= 10 {
            return Ok(None);
        }
        let record = LocalPullAdmission {
            destination: destination.clone(),
            request: request.clone(),
            receipt: None,
            leaf_run_id: None,
            phase: LocalPullPhase::Requested,
            settlement: None,
        };
        write(conn, workspace, &record)?;
        Ok(Some(record))
    })
}

pub(super) fn mutate(
    store: &Store,
    workspace: &str,
    destination: &PullDestination,
    request_id: &str,
    mutation: &LocalPullMutation,
) -> Result<LocalPullAdmission, OrbitError> {
    initialize(store)?;
    store.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
        let conn = tx.connection();
        let mut record = read(conn, workspace, destination, request_id)?.ok_or_else(|| invalid("local pull request missing"))?;
        if record.destination != *destination { return Err(invalid("pull destination changed")); }
        match mutation {
            LocalPullMutation::Receive(receipt) => {
                if let Some(old) = &record.receipt {
                    if old != receipt.as_ref() { return Err(invalid("pull receipt changed")); }
                    return Ok(record);
                }
                if record.phase != LocalPullPhase::Requested || receipt.request != record.request || receipt.machine_id != destination.execution_machine_id {
                    return Err(invalid("pull receipt does not match pending request"));
                }
                if let Some(claim) = &receipt.claim {
                    if claim.executed_on.machine_id != destination.execution_machine_id || claim.request_id != request_id || receipt.task.as_ref().is_none_or(|task| task.id != claim.task_id) {
                        return Err(invalid("pull receipt claim identity mismatch"));
                    }
                    record.phase = LocalPullPhase::Claimed;
                } else {
                    if receipt.task.is_some() { return Err(invalid("idle receipt contains a task")); }
                    record.phase = LocalPullPhase::Idle;
                }
                record.receipt = Some(receipt.as_ref().clone());
            }
            LocalPullMutation::CreateLeaf => {
                if record.leaf_run_id.is_some() { return Ok(record); }
                if record.phase != LocalPullPhase::Claimed { return Err(invalid("leaf creation requires a persisted claim")); }
                let claim = record.receipt.as_ref().and_then(|r| r.claim.as_ref()).ok_or_else(|| invalid("claim missing"))?;
                let now = Utc::now();
                let run_id = next_run_id_conn(conn, workspace, RunIdRole::Child, now)?;
                let job = pipeline(&record.request)?;
                let input = serde_json::json!({"task_ids": [claim.task_id], "base_branch": record.request.ship.base_branch, "base_sync": if job == CLAIMED_LOCAL_PIPELINE {"local"} else {"remote"}});
                let run = JobRun { run_id: run_id.clone(), job_id: job.into(), attempt: 1, state: JobRunState::Pending, scheduled_at: now, started_at: None, finished_at: None, duration_ms: None, created_at: now, pid: None, pid_start_time: None, input: Some(input.clone()), retry_source_run_id: None, knowledge_metrics: None, resolved_crew: None, crew_model: None, steps: vec![], executed_on: Some(claim.executed_on.clone()) };
                let state = PipelineState::new(run_id.clone(), job.into(), input);
                upsert_job_run_for_workspace_conn(conn, workspace, &run, Some(&state))?;
                record.leaf_run_id = Some(run_id);
                record.phase = LocalPullPhase::Created;
            }
            LocalPullMutation::Bound => advance(&mut record, LocalPullPhase::Created, LocalPullPhase::Bound)?,
            LocalPullMutation::LaunchIntent => {
                if record.phase != LocalPullPhase::Bound { return Err(invalid("execution launch uncertain; deliberate recovery required")); }
                let id = record.leaf_run_id.as_deref().ok_or_else(|| invalid("bound leaf missing"))?;
                let run = get_job_run_for_workspace_conn(conn, workspace, id)?.ok_or_else(|| invalid("bound leaf disappeared"))?;
                if run.state != JobRunState::Pending {
                    return Err(invalid("leaf is no longer queued; reconcile before launch"));
                }
                record.phase = LocalPullPhase::Launching;
            }
            LocalPullMutation::Launched => {
                if !matches!(record.phase, LocalPullPhase::Settling | LocalPullPhase::Settled) {
                    advance(&mut record, LocalPullPhase::Launching, LocalPullPhase::Launched)?;
                }
            },
            LocalPullMutation::Settle(settlement) => {
                if !matches!(settlement.as_ref(), ClaimMutation::Fail(_) | ClaimMutation::AcceptHandoff(_)) { return Err(invalid("invalid leaf settlement")); }
                if let Some(old) = &record.settlement {
                    if old != settlement.as_ref() { return Err(invalid("pending settlement is immutable")); }
                    return Ok(record);
                }
                if matches!(record.phase, LocalPullPhase::Requested | LocalPullPhase::Idle | LocalPullPhase::Settled) { return Err(invalid("settlement requires a claim")); }
                record.settlement = Some(settlement.as_ref().clone());
                record.phase = LocalPullPhase::Settling;
            }
            LocalPullMutation::Settled => {
                advance(&mut record, LocalPullPhase::Settling, LocalPullPhase::Settled)?;
                if matches!(record.settlement, Some(ClaimMutation::Fail(_)))
                    && let Some(id) = &record.leaf_run_id {
                        let mut run = get_job_run_for_workspace_conn(conn, workspace, id)?
                            .ok_or_else(|| invalid("bound leaf disappeared before settlement"))?;
                        if !run.state.is_terminal() {
                            run.state = JobRunState::Failed;
                            run.finished_at = Some(Utc::now());
                            upsert_job_run_for_workspace_conn(conn, workspace, &run, None)?;
                        }
                }
            },
        }
        write(conn, workspace, &record)?;
        Ok(record)
    })
}
fn advance(
    record: &mut LocalPullAdmission,
    from: LocalPullPhase,
    to: LocalPullPhase,
) -> Result<(), OrbitError> {
    if record.phase == to {
        return Ok(());
    }
    if record.phase != from {
        return Err(invalid("invalid local pull phase transition"));
    }
    record.phase = to;
    Ok(())
}
