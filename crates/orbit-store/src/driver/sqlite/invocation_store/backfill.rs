//! Reads and writes for recording a dispatch identity after the fact, for
//! invocations that completed before identities were recorded at dispatch.
//! Nothing here runs when the store opens; an operator command drives it.
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::telemetry::AgentExecutionIdentity;

use super::executions::StoredAgentExecutionIdentity;
use crate::Store;
use crate::contracts::{
    AgentExecutionBackfill, InvocationLaunchEvent, UnidentifiedInvocationLaunch,
    UnrecordedLaunchGroup, UnrecordedLaunchOmissions,
};

/// The audit event that records each provider launch and its rendered argv.
pub(crate) const CLI_INVOCATION_STARTED_EVENT: &str = "cli.invocation.started";

impl Store {
    /// Every completed invocation of `workspace_id` without a dispatch
    /// identity, grouped by run and oldest first within it, each with the launch events that may have
    /// started it: same workspace, run and model, launched within
    /// `tolerance` of the invocation's end minus its duration.
    pub fn list_unidentified_invocation_launches(
        &self,
        workspace_id: &str,
        tolerance: chrono::Duration,
    ) -> Result<Vec<UnidentifiedInvocationLaunch>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(
                "SELECT i.id, i.job_run_id, i.activity_id, i.model, i.ts, i.duration_ms, \
                        (SELECT r.resolved_crew FROM job_runs r \
                          WHERE r.workspace_id = i.workspace_id AND r.run_id = i.job_run_id \
                          LIMIT 1) \
                 FROM invocations i \
                 WHERE i.workspace_id = ?1 AND i.agent_execution_id IS NULL \
                 ORDER BY i.job_run_id, i.id",
            )
            .map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map(rusqlite::params![workspace_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(sqlite_store_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)?;
        drop(stmt);

        let mut events_stmt = conn
            .prepare(
                "SELECT event_id, ts, payload_json FROM v2_audit_events \
                 WHERE workspace_id = ?1 AND run_id = ?2 AND event_type = ?3 ORDER BY id",
            )
            .map_err(sqlite_store_error)?;
        // Rows arrive grouped by run, so only one run's launches (whose argv
        // can be long) are held at a time.
        let mut run_launches: (Option<String>, Vec<RecordedLaunch>) = (None, Vec::new());
        let mut unidentified = Vec::with_capacity(rows.len());
        for (invocation_id, run_id, activity_id, model, ended_at, duration_ms, crew) in rows {
            if run_launches.0.as_deref() != Some(run_id.as_str()) {
                let launches = events_stmt
                    .query_map(
                        rusqlite::params![workspace_id, run_id, CLI_INVOCATION_STARTED_EVENT],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        },
                    )
                    .map_err(sqlite_store_error)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(sqlite_store_error)?
                    .into_iter()
                    .filter_map(|(event_id, ts, payload)| {
                        RecordedLaunch::parse(event_id, &ts, &payload)
                    })
                    .collect();
                run_launches = (Some(run_id.clone()), launches);
            }
            // A row whose end time does not parse cannot be placed in time,
            // so it gets no candidates and stays unknown.
            let launched_at = crate::parse_timestamp(&ended_at)
                .ok()
                .map(|ended| ended - chrono::Duration::milliseconds(duration_ms.max(0)));
            let launches = match launched_at {
                Some(launched_at) => run_launches
                    .1
                    .iter()
                    .filter(|launch| {
                        launch.model == model
                            && (launch.event.started_at - launched_at).abs() <= tolerance
                    })
                    .map(|launch| launch.event.clone())
                    .collect(),
                None => Vec::new(),
            };
            unidentified.push(UnidentifiedInvocationLaunch {
                invocation_id,
                run_id,
                activity_id,
                run_resolved_crew: crew.filter(|crew| !crew.trim().is_empty()),
                launches,
            });
        }
        Ok(unidentified)
    }

    /// Record `identity` for one invocation that still has no dispatch
    /// identity, with its backfill provenance, and link the invocation to it.
    /// Returns false, writing nothing, when the invocation is missing or
    /// already has an identity, so running a backfill again changes nothing.
    pub fn record_backfilled_agent_execution(
        &self,
        workspace_id: &str,
        invocation_id: i64,
        launched_at: DateTime<Utc>,
        identity: &AgentExecutionIdentity,
        backfill: &AgentExecutionBackfill,
    ) -> Result<bool, OrbitError> {
        let json = serde_json::to_string(&StoredAgentExecutionIdentity {
            identity: identity.clone(),
            backfill: Some(backfill.clone()),
        })
        .map_err(|error| OrbitError::Store(error.to_string()))?;
        let mut conn = orbit_common::fs::lock_wait::mutex(&self.conn)
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let tx = conn.transaction().map_err(sqlite_store_error)?;
        let invocation = {
            let mut stmt = tx
                .prepare(
                    "SELECT job_run_id, activity_id, COALESCE(provider, agent), model \
                     FROM invocations \
                     WHERE id = ?1 AND workspace_id = ?2 AND agent_execution_id IS NULL",
                )
                .map_err(sqlite_store_error)?;
            let mut rows = stmt
                .query_map(rusqlite::params![invocation_id, workspace_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })
                .map_err(sqlite_store_error)?;
            rows.next().transpose().map_err(sqlite_store_error)?
        };
        let Some((run_id, activity_id, provider, model)) = invocation else {
            return Ok(false);
        };
        tx.execute(
            "INSERT INTO agent_executions(workspace_id, run_id, activity_id, provider, model, identity_json, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                workspace_id,
                run_id,
                activity_id,
                provider,
                model,
                json,
                launched_at.to_rfc3339()
            ],
        )
        .map_err(sqlite_store_error)?;
        let execution_id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE invocations SET agent_execution_id = ?1 WHERE id = ?2 AND agent_execution_id IS NULL",
            rusqlite::params![execution_id, invocation_id],
        )
        .map_err(sqlite_store_error)?;
        tx.commit().map_err(sqlite_store_error)?;
        Ok(true)
    }

    /// Launches of workspace runs, grouped by the step whose audit interval
    /// contains the launch, then by provider and model. Each launch in a
    /// group is recorded on its own. A launch in no step interval, or in
    /// more than one, is not grouped: its step cannot be proven. A launch
    /// that names no provider is not grouped either. Both are counted in
    /// the omissions rather than guessed.
    pub fn list_unrecorded_launch_groups(
        &self,
        workspace_id: &str,
    ) -> Result<(Vec<UnrecordedLaunchGroup>, UnrecordedLaunchOmissions), OrbitError> {
        let conn = self.read()?;
        let crews: std::collections::HashMap<String, Option<String>> = {
            let mut stmt = conn
                .prepare("SELECT run_id, resolved_crew FROM job_runs WHERE workspace_id = ?1")
                .map_err(sqlite_store_error)?;
            let rows = stmt
                .query_map(rusqlite::params![workspace_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                })
                .map_err(sqlite_store_error)?;
            rows.collect::<Result<_, _>>().map_err(sqlite_store_error)?
        };
        let mut launches_stmt = conn
            .prepare(
                "SELECT event_id, ts, payload_json FROM v2_audit_events \
                 WHERE workspace_id = ?1 AND run_id = ?2 AND event_type = ?3 ORDER BY id",
            )
            .map_err(sqlite_store_error)?;
        let mut steps_stmt = conn
            .prepare(
                "SELECT ts, event_type, payload_json FROM v2_audit_events \
                 WHERE workspace_id = ?1 AND run_id = ?2 \
                   AND event_type IN ('step.started', 'step.finished') ORDER BY id",
            )
            .map_err(sqlite_store_error)?;
        let mut runs_stmt = conn
            .prepare(
                "SELECT DISTINCT run_id FROM v2_audit_events \
                 WHERE workspace_id = ?1 AND event_type = ?2 ORDER BY run_id",
            )
            .map_err(sqlite_store_error)?;
        let run_ids: Vec<String> = runs_stmt
            .query_map(
                rusqlite::params![workspace_id, CLI_INVOCATION_STARTED_EVENT],
                |row| row.get::<_, String>(0),
            )
            .map_err(sqlite_store_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_store_error)?;
        drop(runs_stmt);
        let mut groups: Vec<UnrecordedLaunchGroup> = Vec::new();
        let mut omissions = UnrecordedLaunchOmissions::default();
        for run_id in run_ids {
            let Some(crew) = crews.get(&run_id) else {
                continue;
            };
            let launches: Vec<RecordedLaunch> = launches_stmt
                .query_map(
                    rusqlite::params![workspace_id, run_id, CLI_INVOCATION_STARTED_EVENT],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .map_err(sqlite_store_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sqlite_store_error)?
                .into_iter()
                .filter_map(|(event_id, ts, payload)| {
                    RecordedLaunch::parse(event_id, &ts, &payload)
                })
                .collect();
            if launches.is_empty() {
                continue;
            }
            // Step intervals of this run, from its own audit trail.
            let mut intervals: Vec<StepInterval> = Vec::new();
            let step_rows = steps_stmt
                .query_map(rusqlite::params![workspace_id, run_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(sqlite_store_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sqlite_store_error)?;
            for (ts, event_type, payload) in step_rows {
                let Ok(at) = crate::parse_timestamp(&ts) else {
                    continue;
                };
                let Ok(payload) = serde_json::from_str::<serde_json::Value>(&payload) else {
                    continue;
                };
                let Some(step_id) = payload
                    .get("step_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                else {
                    continue;
                };
                if event_type == "step.started" {
                    intervals.push(StepInterval {
                        step_id,
                        started_at: at,
                        finished_at: None,
                    });
                } else if let Some(open) = intervals
                    .iter_mut()
                    .rev()
                    .find(|open| open.step_id == step_id && open.finished_at.is_none())
                {
                    open.finished_at = Some(at);
                }
            }
            for launch in launches {
                let containing: Vec<&StepInterval> = intervals
                    .iter()
                    .filter(|interval| {
                        launch.event.started_at >= interval.started_at
                            && interval
                                .finished_at
                                .is_none_or(|end| launch.event.started_at <= end)
                    })
                    .collect();
                let [only] = containing.as_slice() else {
                    omissions.no_step += 1;
                    continue;
                };
                let Some(provider) = launch.provider.clone() else {
                    omissions.no_provider += 1;
                    continue;
                };
                let key = (&only.step_id, &provider, &launch.model);
                if let Some(group) = groups
                    .iter_mut()
                    .find(|group: &&mut UnrecordedLaunchGroup| {
                        group.run_id == run_id
                            && group.activity_id == *key.0
                            && group.provider == *key.1
                            && group.model == *key.2
                    })
                {
                    group.launches.push(launch.event);
                } else {
                    groups.push(UnrecordedLaunchGroup {
                        run_id: run_id.clone(),
                        activity_id: only.step_id.clone(),
                        provider,
                        model: launch.model.clone(),
                        run_resolved_crew: crew.clone().filter(|crew| !crew.trim().is_empty()),
                        launches: vec![launch.event],
                    });
                }
            }
        }
        Ok((groups, omissions))
    }

    /// Record one launch of a run whose invocation never completed.
    ///
    /// Returns false, writing nothing, when a row already covers this
    /// launch. A backfilled row covers it when its provenance lists this
    /// launch's event and either lists no other event or is stamped with
    /// this launch's start (an older row that merged several launches
    /// covers only the launch it was stamped with). A dispatch-time row,
    /// which has no provenance, covers the one sibling launch of the same
    /// run, step, provider and model whose start is within `tolerance` and
    /// is the closest to the row. `group.launches` is that full sibling
    /// set, including a launch whose effort cannot be read: an unreadable
    /// audit does not move the row onto a later launch, and this function
    /// never infers an effort for it. A later launch of the same step is
    /// its own row, and running the backfill again writes nothing new.
    pub fn record_backfilled_launch_execution(
        &self,
        workspace_id: &str,
        group: &UnrecordedLaunchGroup,
        launch: &InvocationLaunchEvent,
        tolerance: chrono::Duration,
        identity: &AgentExecutionIdentity,
        backfill: &AgentExecutionBackfill,
    ) -> Result<bool, OrbitError> {
        let (run_id, activity_id, provider, model) = (
            group.run_id.as_str(),
            group.activity_id.as_str(),
            group.provider.as_str(),
            group.model.as_deref(),
        );
        let json = serde_json::to_string(&StoredAgentExecutionIdentity {
            identity: identity.clone(),
            backfill: Some(backfill.clone()),
        })
        .map_err(|error| OrbitError::Store(error.to_string()))?;
        let mut conn = orbit_common::fs::lock_wait::mutex(&self.conn)
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let tx = conn.transaction().map_err(sqlite_store_error)?;
        let existing = {
            let mut stmt = tx
                .prepare(
                    "SELECT identity_json, started_at FROM agent_executions \
                     WHERE workspace_id = ?1 AND run_id = ?2 AND activity_id = ?3 \
                       AND provider = ?4 AND model IS ?5",
                )
                .map_err(sqlite_store_error)?;
            let rows = stmt
                .query_map(
                    rusqlite::params![workspace_id, run_id, activity_id, provider, model],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .map_err(sqlite_store_error)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(sqlite_store_error)?
        };
        for (identity_json, started_at) in &existing {
            if launch_row_covers(
                identity_json,
                started_at,
                launch,
                &group.launches,
                tolerance,
            )? {
                return Ok(false);
            }
        }
        tx.execute(
            "INSERT INTO agent_executions(workspace_id, run_id, activity_id, provider, model, identity_json, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                workspace_id,
                run_id,
                activity_id,
                provider,
                model,
                json,
                launch.started_at.to_rfc3339()
            ],
        )
        .map_err(sqlite_store_error)?;
        tx.commit().map_err(sqlite_store_error)?;
        Ok(true)
    }
}

/// Whether one stored row already covers `launch`.
fn launch_row_covers(
    identity_json: &str,
    started_at: &str,
    launch: &InvocationLaunchEvent,
    siblings: &[InvocationLaunchEvent],
    tolerance: chrono::Duration,
) -> Result<bool, OrbitError> {
    let stored: StoredAgentExecutionIdentity = serde_json::from_str(identity_json)
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let row_started = crate::parse_timestamp(started_at).map_err(sqlite_store_error)?;
    if let Some(backfill) = &stored.backfill {
        return Ok(backfill_row_covers_launch(backfill, row_started, launch));
    }
    Ok(dispatch_row_covers_launch(
        row_started,
        launch,
        siblings,
        tolerance,
    ))
}

/// A backfilled row covers the launch it lists when that list is this
/// launch alone, or when the row was stamped with this launch's start.
/// A row that lists several launches covers only the one it was stamped
/// with, so the others can be recorded on their own.
fn backfill_row_covers_launch(
    backfill: &AgentExecutionBackfill,
    row_started: DateTime<Utc>,
    launch: &InvocationLaunchEvent,
) -> bool {
    if !backfill
        .event_ids
        .iter()
        .any(|event_id| event_id == &launch.event_id)
    {
        return false;
    }
    backfill.event_ids.len() == 1 || row_started == launch.started_at
}

/// A dispatch-time row has no launch event. It covers the closest sibling
/// launch within `tolerance`, one row to one launch. `siblings` includes
/// launches whose effort cannot be read; the closest one owns the row, and
/// this match does not infer an effort. Equal distances keep the earlier
/// start, then the lower event id.
fn dispatch_row_covers_launch(
    row_started: DateTime<Utc>,
    launch: &InvocationLaunchEvent,
    siblings: &[InvocationLaunchEvent],
    tolerance: chrono::Duration,
) -> bool {
    let distance = (row_started - launch.started_at).abs();
    if distance > tolerance {
        return false;
    }
    siblings.iter().all(|other| {
        if other.event_id == launch.event_id {
            return true;
        }
        let other_distance = (row_started - other.started_at).abs();
        if other_distance > tolerance || other_distance > distance {
            return true;
        }
        if other_distance < distance {
            return false;
        }
        (other.started_at, other.event_id.as_str()) >= (launch.started_at, launch.event_id.as_str())
    })
}

/// One step's audit interval in a run: `step.started` to its `step.finished`.
struct StepInterval {
    step_id: String,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
}

/// A launch event with the model it launched, before matching.
struct RecordedLaunch {
    model: Option<String>,
    provider: Option<String>,
    event: InvocationLaunchEvent,
}

impl RecordedLaunch {
    /// A payload with neither an argv nor a recorded effort is no evidence:
    /// launch events before argv capture (2026-09-10) and before the
    /// recorded effort (2026-10-29) carry neither, and stay unknown.
    fn parse(event_id: String, ts: &str, payload: &str) -> Option<Self> {
        let started_at = crate::parse_timestamp(ts).ok()?;
        let payload: serde_json::Value = serde_json::from_str(payload).ok()?;
        let argv_redacted = match payload.get("argv_redacted") {
            Some(argv) => Some(
                argv.as_array()?
                    .iter()
                    .map(|arg| arg.as_str().map(str::to_string))
                    .collect::<Option<Vec<_>>>()?,
            ),
            None => None,
        };
        let (reasoning_effort, effort_unreadable) = match payload
            .get("reasoning_effort")
            .and_then(serde_json::Value::as_str)
        {
            Some(raw) => match raw.parse::<orbit_types::identity::ReasoningEffort>() {
                Ok(effort) => (Some(effort), false),
                Err(_) => (None, true),
            },
            None => (None, false),
        };
        if argv_redacted.is_none() && reasoning_effort.is_none() && !effort_unreadable {
            return None;
        }
        let model = payload
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let provider = payload
            .get("provider")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        Some(Self {
            model,
            provider,
            event: InvocationLaunchEvent {
                event_id,
                started_at,
                argv_redacted,
                reasoning_effort,
                effort_unreadable,
            },
        })
    }
}
