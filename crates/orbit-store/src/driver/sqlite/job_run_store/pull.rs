//! Durable caller admissions share the job database writer transaction. Receipt
//! replay never creates a second leaf, including after the first leaf terminates.
use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::workflow::{
    JobRun, JobRunState, PipelineState, REVIEW_ADMISSION_KEY, ReviewAdmission, ReviewTiming,
    RunIdRole,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::queries::{
    get_job_run_for_workspace_conn, next_run_id_conn, upsert_job_run_for_workspace_conn,
};
use super::state::read_state_json_conn;
use crate::Store;
use crate::contracts::{
    AdmissionRequest, ClaimMutation, ClaimRepair, DrainLeafOccupancy, LocalPullAdmission,
    LocalPullMutation, LocalPullPhase, PullDestination,
};
use crate::driver::sqlite::migration::FeatureMigration;

/// This feature's schema ledger name.
pub(crate) const FEATURE: &str = "local_pull";

/// Append-only schema registry for this feature.
pub(crate) const MIGRATIONS: &[FeatureMigration] = &[FeatureMigration::new(
    1,
    "pending_requests_and_unique_leaves",
    |conn| {
        conn.execute_batch("CREATE TABLE local_pull_admissions (
            workspace_id TEXT NOT NULL, owner_machine TEXT NOT NULL,
            owner_workspace TEXT NOT NULL, execution_machine TEXT NOT NULL,
            request_id TEXT NOT NULL, claim_id TEXT, leaf_run_id TEXT, record_json TEXT NOT NULL,
            PRIMARY KEY(workspace_id, owner_machine, owner_workspace, execution_machine, request_id),
            UNIQUE(workspace_id, owner_machine, owner_workspace, claim_id),
            UNIQUE(workspace_id, leaf_run_id));")
            .map_err(db_error)
    },
)];

fn initialize(store: &Store) -> Result<(), OrbitError> {
    store.apply_feature_migrations(FEATURE, MIGRATIONS)
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
/// Whether the `local_pull` feature migration has ever run here. A reader must
/// not create the feature schema merely to discover there are no admissions.
fn admissions_table_exists(conn: &Connection) -> Result<bool, OrbitError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='local_pull_admissions')",
        [],
        |row| row.get(0),
    )
    .map_err(db_error)
}

/// Serialized names (`LocalPullPhase` is `snake_case`) of the phases that no
/// longer hold a drain slot: an idle poll, a settled claim, a refused request.
/// SQL narrows reads to the rest so the occupancy check inside the writer
/// transaction, and every settle-only pass, never decodes finished history.
/// [`LocalPullAdmission::holds_capacity`] stays the authority: rows are
/// re-filtered by it, so a phase missing here is read too much, never lost.
const RELEASED_PHASES_SQL: &str = "('idle','settled','refused')";

/// Terminal `Idle` and `Refused` rows one workspace keeps. A drain that polls
/// an owner with nothing ready leaves one such row per poll (about 1,440 a day
/// at the shipped 60 second idle sleep), and no reader needs an old one: they
/// hold no claim, and their request IDs are random, never reused. This keeps
/// roughly a week of one drain's polls for diagnosis. `Settled` rows are never
/// pruned — the failure breaker and status reporting read them.
pub(crate) const TERMINAL_ROWS_RETAINED: usize = 10_000;

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
/// Admissions that still hold a slot, in admission order.
fn holding_records(
    conn: &Connection,
    workspace: &str,
) -> Result<Vec<LocalPullAdmission>, OrbitError> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT record_json FROM local_pull_admissions WHERE workspace_id=?1 \
             AND json_extract(record_json,'$.phase') NOT IN {RELEASED_PHASES_SQL} ORDER BY rowid"
        ))
        .map_err(db_error)?;
    let raw = stmt
        .query_map([workspace], |r| r.get::<_, String>(0))
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    let mut holding = Vec::with_capacity(raw.len());
    for record in raw.into_iter().map(decode) {
        let record = record?;
        if record.holds_capacity() {
            holding.push(record);
        }
    }
    Ok(holding)
}

/// Drop all but the newest [`TERMINAL_ROWS_RETAINED`] `Idle` and `Refused`
/// rows of `workspace`, oldest first by insertion order.
fn prune_terminal_rows(conn: &Connection, workspace: &str) -> Result<(), OrbitError> {
    // A cheap count keeps the common pass from scanning JSON at all.
    let rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM local_pull_admissions WHERE workspace_id=?1",
            [workspace],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    let retained = i64::try_from(TERMINAL_ROWS_RETAINED).unwrap_or(i64::MAX);
    if rows <= retained {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM local_pull_admissions WHERE workspace_id=?1 \
         AND json_extract(record_json,'$.phase') IN ('idle','refused') \
         AND rowid NOT IN (SELECT rowid FROM local_pull_admissions WHERE workspace_id=?1 \
             AND json_extract(record_json,'$.phase') IN ('idle','refused') \
             ORDER BY rowid DESC LIMIT ?2)",
        params![workspace, retained],
    )
    .map_err(db_error)?;
    Ok(())
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
        if !admissions_table_exists(conn)? { return Ok(None); }
        conn.query_row("SELECT record_json FROM local_pull_admissions WHERE workspace_id=?1 AND leaf_run_id=?2",
            params![workspace, run_id], |row| row.get::<_, String>(0))
            .optional().map_err(db_error)?.map(decode).transpose()
    })
}

pub(super) fn list(store: &Store, workspace: &str) -> Result<Vec<LocalPullAdmission>, OrbitError> {
    initialize(store)?;
    store.with_read_connection(|conn| records(conn, workspace))
}

/// Admissions still holding a slot, without creating the feature schema: a
/// settle-only pass runs from `orbit run cancel` and `orbit run auto --stop`
/// in workspaces that may never have pulled [ORB-13663].
pub(super) fn unsettled(
    store: &Store,
    workspace: &str,
) -> Result<Vec<LocalPullAdmission>, OrbitError> {
    store.with_read_connection(|conn| {
        if !admissions_table_exists(conn)? {
            return Ok(Vec::new());
        }
        holding_records(conn, workspace)
    })
}

/// How many of `run_id`'s most recent settled claims against `destination`
/// failed in a row, without creating the feature schema.
///
/// The breaker that stops a failing drain reads this on every pass, so it must
/// not decode the whole table: SQL narrows to this drain's settled claims,
/// newest first, and the walk stops at the first one that did not fail. A claim
/// closed obsolete (settled with an owner refusal) says nothing about this
/// executor, so it neither extends nor resets the streak; nor does a claim a
/// cancel released back to the owner.
///
/// `claim_id IS NOT NULL` is the cheap column test that lets SQLite skip the
/// idle polls and refused requests, the rows that pile up unbounded, without
/// parsing their JSON: a settlement requires a claim, so a settled row always
/// carries one.
pub(super) fn consecutive_failed_settlements(
    store: &Store,
    workspace: &str,
    destination: &PullDestination,
    run_id: &str,
) -> Result<usize, OrbitError> {
    store.with_read_connection(|conn| {
        if !admissions_table_exists(conn)? {
            return Ok(0);
        }
        let mut stmt = conn
            .prepare(
                "SELECT record_json FROM local_pull_admissions \
                 WHERE workspace_id=?1 AND owner_machine=?2 AND owner_workspace=?3 \
                 AND execution_machine=?4 AND claim_id IS NOT NULL \
                 AND json_extract(record_json,'$.phase')='settled' \
                 AND json_extract(record_json,'$.request.run_context.run_id')=?5 \
                 AND json_extract(record_json,'$.destination.selector')=?6 \
                 ORDER BY rowid DESC",
            )
            .map_err(db_error)?;
        let mut rows = stmt
            .query(params![
                workspace,
                destination.owner_machine_id,
                destination.owner_workspace_id,
                destination.execution_machine_id,
                run_id,
                destination.selector,
            ])
            .map_err(db_error)?;
        let mut streak = 0;
        while let Some(row) = rows.next().map_err(db_error)? {
            let record = decode(row.get::<_, String>(0).map_err(db_error)?)?;
            // SQL only narrows; the record stays the authority.
            if record.destination != *destination
                || record.request.run_context.run_id != run_id
                || record.phase != LocalPullPhase::Settled
                || record.refusal.is_some()
            {
                continue;
            }
            match record.settlement {
                Some(ClaimMutation::Fail(_)) => streak += 1,
                // A claim handed back by a cancel says nothing about the owner.
                Some(ClaimMutation::Release(_)) => continue,
                _ => break,
            }
        }
        Ok(streak)
    })
}

/// Every claimed admission `run_id` made, any phase, in admission order.
/// `claim_id IS NOT NULL` skips the idle polls and refused requests without
/// parsing their JSON, as [`consecutive_failed_settlements`] does.
pub(super) fn claims_admitted_by(
    store: &Store,
    workspace: &str,
    run_id: &str,
) -> Result<Vec<LocalPullAdmission>, OrbitError> {
    store.with_read_connection(|conn| {
        if !admissions_table_exists(conn)? {
            return Ok(Vec::new());
        }
        let mut stmt = conn
            .prepare(
                "SELECT record_json FROM local_pull_admissions \
                 WHERE workspace_id=?1 AND claim_id IS NOT NULL \
                 AND json_extract(record_json,'$.request.run_context.run_id')=?2 \
                 ORDER BY rowid",
            )
            .map_err(db_error)?;
        let raw = stmt
            .query_map(params![workspace, run_id], |r| r.get::<_, String>(0))
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        let mut claims = Vec::with_capacity(raw.len());
        for record in raw.into_iter().map(decode) {
            let record = record?;
            // SQL only narrows; the record stays the authority.
            if record.request.run_context.run_id == run_id {
                claims.push(record);
            }
        }
        Ok(claims)
    })
}

/// How far the dispatch lineage walk follows records. A loop guard for
/// a malformed or cyclic dispatch chain, not a tuning knob.
const MAX_DISPATCH_LINEAGE_DEPTH: usize = 64;

/// Every run a wrapper or coordinator dispatched, transitively. Terminal
/// dispatch records still identify live descendants.
fn dispatch_lineage(
    conn: &Connection,
    workspace: &str,
    root_state: Option<&String>,
) -> Result<BTreeSet<String>, OrbitError> {
    let mut frontier = vec![root_state.cloned()];
    let mut seen = BTreeSet::new();
    for _ in 0..MAX_DISPATCH_LINEAGE_DEPTH {
        let mut next = Vec::new();
        for raw in frontier.into_iter().flatten() {
            let state: PipelineState = serde_json::from_str(&raw).map_err(db_error)?;
            for child in state.child_dispatches {
                if seen.insert(child.child_run_id.clone()) {
                    next.push(read_state_json_conn(conn, workspace, &child.child_run_id)?);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    Ok(seen)
}

/// The one capacity reading both admission paths allocate against [ORB-12617].
///
/// Legacy wrappers are counted once and replaced by whichever descendant is
/// actually carrying their work: a live leaf run of any of the four leaf
/// definitions, or a pull admission whose bound leaf has gone terminal but has
/// not settled yet. An admission with no live run of its own — never created,
/// or created and since terminal — holds its own slot instead, so a slot is
/// released exactly when the claim settles and not before.
fn occupancy(
    conn: &Connection,
    workspace: &str,
    coordinator: Option<&str>,
) -> Result<DrainLeafOccupancy, OrbitError> {
    let mut stmt = conn
        .prepare(
            "SELECT r.run_id, r.job_id, s.pipeline_state_json FROM job_runs r \
             LEFT JOIN job_run_states s ON s.workspace_id = r.workspace_id AND s.run_id = r.run_id \
             WHERE r.workspace_id=?1 AND r.state IN ('pending','running','retrying')",
        )
        .map_err(db_error)?;
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
    // Admissions that still hold capacity. Read before the wrapper walk
    // because a wrapper whose lineage reaches one of their leaves is
    // represented by that admission, terminal leaf or not.
    let pending = if admissions_table_exists(conn)? {
        holding_records(conn, workspace)?
    } else {
        Vec::new()
    };
    let admitted_runs: BTreeSet<String> = pending
        .iter()
        .filter_map(|record| record.leaf_run_id.clone())
        .collect();

    let mut pipelines: BTreeMap<String, usize> = BTreeMap::new();
    let leaves: BTreeSet<String> = active
        .iter()
        .filter(|(_, job, _)| is_leaf_pipeline(job))
        .map(|(id, _, _)| id.clone())
        .collect();
    let mut slots: BTreeSet<String> = leaves.iter().cloned().collect();
    for (id, job, state) in &active {
        if is_leaf_pipeline(job) {
            *pipelines.entry(job.clone()).or_insert(0) += 1;
        } else if job == LEGACY_WRAPPER_PIPELINE {
            let seen = dispatch_lineage(conn, workspace, state.as_ref())?;
            if seen.is_disjoint(&leaves) && seen.is_disjoint(&admitted_runs) {
                slots.insert(id.clone());
            }
        }
    }

    // Dispatch history remains authoritative even after a wrapper or an old
    // coordinator finishes. Walking it avoids counting a wrapper and its
    // delivery twice, or attributing our detached delivery to another drain.
    let mut owned = if let Some(run_id) = coordinator {
        let raw = read_state_json_conn(conn, workspace, run_id)?;
        dispatch_lineage(conn, workspace, raw.as_ref())?
    } else {
        BTreeSet::new()
    };
    let mut owned_unrepresented = 0;
    let mut occupied = slots.len();
    for record in pending {
        if record
            .leaf_run_id
            .as_ref()
            .is_none_or(|id| !slots.contains(id))
        {
            occupied += 1;
            if coordinator == Some(record.request.run_context.run_id.as_str()) {
                owned_unrepresented += 1;
            }
        }
        if coordinator == Some(record.request.run_context.run_id.as_str())
            && let Some(id) = record.leaf_run_id.as_ref()
        {
            owned.insert(id.clone());
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
    Ok(DrainLeafOccupancy {
        occupied,
        per_pipeline: pipelines,
        inherited: coordinator.map(|_| {
            occupied.saturating_sub(slots.intersection(&owned).count() + owned_unrepresented)
        }),
    })
}

/// The shared reading both admission paths allocate against [ORB-12617].
///
/// Read-only and schema-neutral: a workspace that has never pulled has no
/// `local_pull` feature schema, and asking how full it is must not create one.
pub(super) fn drain_occupancy(
    store: &Store,
    workspace: &str,
    coordinator: Option<&str>,
) -> Result<DrainLeafOccupancy, OrbitError> {
    store.with_read_connection(|conn| occupancy(conn, workspace, coordinator))
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

/// The before-PR review admission a claimed leaf runs under: the owner's
/// captured contract, when the ship contract carries one [ORB-13908].
fn claim_review_admission(
    request: &AdmissionRequest,
    now: chrono::DateTime<Utc>,
) -> Option<ReviewAdmission> {
    let review = request.ship.review.as_ref()?;
    Some(ReviewAdmission {
        contract_version: review.contract_version,
        // The claim's contract is resolved by the owner and carries no
        // operation policy version of its own.
        policy_version: 0,
        timing: ReviewTiming::BeforePr,
        timing_source: CLAIM_SOURCE.into(),
        crew: review.crew.clone(),
        crew_source: CLAIM_SOURCE.into(),
        budget: review.budget,
        required_validation_commands: review.required_validation_commands.clone(),
        captured_at: now,
    })
}

/// Provenance label of a review admission seeded from a claim.
const CLAIM_SOURCE: &str = "claim";

/// The claimed leaf input naming the candidate a repair claim restores.
const CLAIM_REPAIR_KEY: &str = "claim_repair";

/// What a repair claim's leaf needs to restore its preserved candidate: the
/// published branch and head, the base it was validated on, and why its
/// landing stopped.
fn claim_repair_input(repair: &ClaimRepair) -> serde_json::Value {
    serde_json::json!({
        "repairs_claim_id": repair.repairs_claim_id,
        "handoff_id": repair.handoff_id,
        "branch": repair.candidate.source_branch,
        "head_sha": repair.candidate.candidate.commit,
        "base_sha": repair.candidate.base.commit,
        "stop_evidence": repair.stop_evidence,
    })
}

pub(crate) const CLAIMED_PR_PIPELINE: &str = "task_claimed_pr_pipeline";
pub(crate) const CLAIMED_LOCAL_PIPELINE: &str = "task_claimed_local_pipeline";

/// The loose-leaf wrapper the legacy drain dispatches. It occupies a slot on
/// behalf of the leaf beneath it, so it is only counted while nothing beneath
/// it is.
const LEGACY_WRAPPER_PIPELINE: &str = "task_auto_pipeline";

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
    if request.ship.before_pr && !(request.review_gate && request.ship.mode == "pr") {
        return Err(invalid(
            "an owner with review.before_pr on admits only a PR leaf that runs the before-PR gate",
        ));
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
        let raw = read_state_json_conn(conn, workspace, &parent.run_id)?;
        let state: PipelineState =
            serde_json::from_str(&raw.ok_or_else(|| invalid("pull drain state missing"))?)
                .map_err(db_error)?;
        if state.admissions_stopped() {
            return Ok(None);
        }
        let ceiling = state
            .effective_max_active_leaf_runs(u32::try_from(ceiling).unwrap_or(u32::MAX))
            as usize;
        // The drain's worker limit is the only ceiling: the leaf definitions
        // declare no active-run limit of their own [ORB-13893].
        if occupancy(conn, workspace, None)?.occupied >= ceiling {
            return Ok(None);
        }
        let record = LocalPullAdmission {
            destination: destination.clone(),
            request: request.clone(),
            receipt: None,
            leaf_run_id: None,
            phase: LocalPullPhase::Requested,
            settlement: None,
            refusal: None,
            settlement_refusal: None,
        };
        write(conn, workspace, &record)?;
        prune_terminal_rows(conn, workspace)?;
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
                // The claimed task lives in the owner's store, not this one, so
                // the leaf carries the owner's snapshot of it. Crew resolution
                // (`orbit-core` `task_crew_from_run_input`) reads `claimed_task`
                // in place of a local task lookup, so the leaf runs on the
                // owner task's crew rather than this host's `default_crew`.
                let task = record.receipt.as_ref().and_then(|r| r.task.as_ref()).filter(|task| task.id == claim.task_id).ok_or_else(|| invalid("claimed task snapshot missing"))?;
                let mut input = serde_json::json!({"task_ids": [claim.task_id], "base_branch": record.request.ship.base_branch, "base_sync": if job == CLAIMED_LOCAL_PIPELINE {"local"} else {"remote"}, "claimed_task": {"id": task.id, "crew": task.crew}});
                // The leaf's review admission is the claim's captured contract,
                // never this host's settings [ORB-13908].
                if let (Some(review), Some(object)) = (claim_review_admission(&record.request, now), input.as_object_mut()) {
                    object.insert(REVIEW_ADMISSION_KEY.into(), serde_json::to_value(review).map_err(db_error)?);
                }
                // [ORB-14257] The candidate an earlier claim preserved, for the
                // leaf's `resume_candidate` step to carry onto this base; a
                // claimed-local leaf continues one too [ORB-14338].
                if let (Some(candidate), Some(object)) = (&task.resume_candidate, input.as_object_mut()) {
                    object.insert("resume_candidate".into(), serde_json::to_value(candidate).map_err(db_error)?);
                }
                // A repair claim's leaf restores the candidate its stopped
                // landing preserved rather than implementing afresh [ORB-14261].
                if let (Some(repair), Some(object)) = (&claim.repair, input.as_object_mut()) {
                    object.insert(CLAIM_REPAIR_KEY.into(), claim_repair_input(repair));
                }
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
                if !matches!(settlement.as_ref(), ClaimMutation::Fail(_) | ClaimMutation::Release(_) | ClaimMutation::AcceptHandoff(_)) { return Err(invalid("invalid leaf settlement")); }
                if let Some(old) = &record.settlement {
                    if old != settlement.as_ref() { return Err(invalid("pending settlement is immutable")); }
                    return Ok(record);
                }
                if matches!(record.phase, LocalPullPhase::Requested | LocalPullPhase::Idle | LocalPullPhase::Settled | LocalPullPhase::Refused) { return Err(invalid("settlement requires a claim")); }
                record.settlement = Some(settlement.as_ref().clone());
                record.phase = LocalPullPhase::Settling;
            }
            LocalPullMutation::Settled | LocalPullMutation::SettleObsolete(_) => {
                if let LocalPullMutation::SettleObsolete(reason) = mutation {
                    if record.phase == LocalPullPhase::Settled { return Ok(record); }
                    if record.phase != LocalPullPhase::Settling {
                        return Err(invalid("only a pending settlement can be closed as obsolete"));
                    }
                    record.refusal = Some(reason.clone());
                }
                advance(&mut record, LocalPullPhase::Settling, LocalPullPhase::Settled)?;
                // A refusal the owner answered earlier no longer stands.
                record.settlement_refusal = None;
                // A settled claim closes a leaf that never started. A leaf that
                // is running is left alone: it finishes, or a forced cancel
                // stops its process first; failing it here would orphan a live
                // worker.
                let closed = match record.settlement {
                    Some(ClaimMutation::Fail(_)) => Some(JobRunState::Failed),
                    Some(ClaimMutation::Release(_)) => Some(JobRunState::Cancelled),
                    _ => None,
                };
                if let Some(closed) = closed
                    && let Some(id) = &record.leaf_run_id {
                        let mut run = get_job_run_for_workspace_conn(conn, workspace, id)?
                            .ok_or_else(|| invalid("bound leaf disappeared before settlement"))?;
                        if run.state == JobRunState::Pending {
                            run.state = closed;
                            run.finished_at = Some(Utc::now());
                            upsert_job_run_for_workspace_conn(conn, workspace, &run, None)?;
                        }
                }
            },
            LocalPullMutation::DeferSettlement(refusal) => {
                if record.phase != LocalPullPhase::Settling {
                    return Err(invalid("only a pending settlement can be deferred"));
                }
                record.settlement_refusal = Some(refusal.clone());
            }
            LocalPullMutation::Refuse(reason) => {
                if record.phase == LocalPullPhase::Refused { return Ok(record); }
                if record.phase != LocalPullPhase::Requested || record.receipt.is_some() {
                    return Err(invalid("only an unanswered request can be closed as refused"));
                }
                record.phase = LocalPullPhase::Refused;
                record.refusal = Some(reason.clone());
            }
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
