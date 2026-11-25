//! The frozen automation feature registry, with private copies of the
//! one-time member repairs its v4 and v5 entries run. The repairs read and
//! rewrite persisted state with the current domain types, as a data backfill
//! may; the live member checkpoint code no longer shares them.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::workflow::automation::AutomationState;
use orbit_types::workflow::automation::members::{MemberAttempt, MemberState, StateTriggerKind};
use orbit_types::workflow::{JobRunState, PipelineState};
use rusqlite::{Connection, params};

use super::FeatureMigration;

pub(super) const FEATURE: &str = "automation";

pub(super) const MIGRATIONS: &[FeatureMigration] = &[
    FeatureMigration::breaking(1, "consumer_checkpoints_and_coverage", |conn| {
        conn.execute_batch("CREATE TABLE automation_consumers (consumer TEXT PRIMARY KEY, generation INTEGER NOT NULL, state_json TEXT NOT NULL);
            CREATE TABLE automation_coverage (batch_id TEXT PRIMARY KEY, consumer TEXT NOT NULL, batch_json TEXT NOT NULL, receipt_json TEXT NOT NULL, accepted_at TEXT NOT NULL);
            CREATE INDEX automation_coverage_consumer ON automation_coverage(consumer, accepted_at);
            CREATE TABLE automation_delivery_intents (record_id TEXT PRIMARY KEY, repository TEXT NOT NULL, branch TEXT NOT NULL, delivery_json TEXT NOT NULL);
            CREATE TABLE automation_delivery_members (repository TEXT NOT NULL, branch TEXT NOT NULL, commit_id TEXT NOT NULL, record_id TEXT NOT NULL, PRIMARY KEY(repository,branch,commit_id,record_id));
            CREATE TABLE automation_waivers (batch_id TEXT PRIMARY KEY, consumer TEXT NOT NULL, batch_json TEXT NOT NULL, waiver_json TEXT NOT NULL);
            CREATE TABLE automation_job_keys (workspace_id TEXT NOT NULL, action_key TEXT NOT NULL, run_id TEXT NOT NULL, PRIMARY KEY(workspace_id,action_key));")
                    .map_err(sqlite_store_error)
    }),
    FeatureMigration::breaking(2, "retry_lineage_index", |conn| {
        conn.execute_batch(
                    "CREATE INDEX IF NOT EXISTS job_runs_retry_lineage ON job_runs(workspace_id,retry_source_run_id)",
                )
                .map_err(sqlite_store_error)
    }),
    FeatureMigration::breaking(3, "consumer_recovery_records", |conn| {
        conn.execute_batch(
                    "CREATE TABLE automation_recoveries (record_id TEXT PRIMARY KEY, consumer TEXT NOT NULL, recorded_at TEXT NOT NULL, record_json TEXT NOT NULL);
            CREATE INDEX automation_recoveries_consumer ON automation_recoveries(consumer, recorded_at);",
                )
                .map_err(sqlite_store_error)
    }),
    FeatureMigration::breaking(
        4,
        "release_stale_source_failures",
        release_stale_source_failures,
    ),
    // Rewrites member state rows into the shape older binaries already read
    // and write, and advances each repaired consumer's generation, so an
    // older writer holding the old snapshot is refused rather than misread
    // [ORB-15260].
    FeatureMigration::data_only(
        5,
        "release_misread_pilot_failures",
        release_misread_pilot_failures,
    ),
];

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
fn release_stale_source_failures(conn: &Connection) -> Result<(), OrbitError> {
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
fn release_misread_pilot_failures(conn: &Connection) -> Result<(), OrbitError> {
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
            .map_err(sqlite_store_error)?;
        statement
            .query_map([run_id], |row| row.get::<_, String>(0))
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(sqlite_store_error)?
    };
    Ok(!states.is_empty()
        && states.iter().all(|raw| {
            serde_json::from_str::<PipelineState>(raw).is_ok_and(|state| {
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
            .map_err(sqlite_store_error)?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(sqlite_store_error)?
    };

    for (consumer, generation, raw) in rows {
        // A record this binary cannot read is left for its own reader.
        let Ok(mut state) = serde_json::from_str::<AutomationState>(&raw) else {
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
        let encoded =
            serde_json::to_string(&state).map_err(|e| OrbitError::Store(e.to_string()))?;
        conn.execute(
            "UPDATE automation_consumers SET generation=?1, state_json=?2 WHERE consumer=?3 AND generation=?4",
            params![state.generation, encoded, consumer, generation],
        )
        .map_err(sqlite_store_error)?;
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
