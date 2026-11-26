//! One-shot backfill of the dispatch identity of invocations that completed
//! before Orbit recorded identities at dispatch.
//!
//! Every provider launch is audited as a `cli.invocation.started` event whose
//! redacted argv carries the effort flag Orbit rendered, and which since
//! 2026-10-29 also records the resolved effort directly. For each invocation
//! with no dispatch identity, the backfill finds the launch events of its
//! workspace, run and model that started within [`LAUNCH_MATCH_TOLERANCE`] of
//! the invocation's end minus its duration, reads the effort from the recorded
//! effort or the argv (both must agree when both are present) and records it
//! as a dispatch identity marked as backfilled. For a run whose invocation
//! never completed there is no invocation to record on, so it records each
//! readable launch on its own, stamped with that launch's time and attributed to the
//! step its causal links name (the launch's parent `activity.started`, whose
//! parent is the `step.started`; a run-level activity such as `final_recovery`
//! by its own name), else to the one step whose audit interval contains it
//! [ORB-24150]. A later launch of the same
//! step is its own record. A launch a dispatch-time row already covers — the
//! closest sibling of the same run, step, provider and model within a minute,
//! including a sibling whose effort cannot be read — is left as that row. An
//! unreadable sibling can own that row and is never assigned an inferred
//! effort. It never runs when a store opens; an operator runs
//! it, on any host, against that host's own store.
//!
//! An invocation stays unrecorded (and its effort `unknown`) when no launch
//! event matches (including events that predate both effort sources), or when
//! the matching events disagree on the effort or carry an effort this binary
//! cannot read. A launch stays unrecorded when neither its causal links nor a
//! single step interval prove its step,
//! when it names no provider, or when that launch's own effort cannot be
//! read; another launch of the same step is recorded on its own.
//! `cli.invocation.activity` events carry no argv or effort and are never a
//! source. Effort is never taken from crew configuration or a crew name.
//! Invocations that already have an identity are never touched, so running
//! it again changes nothing.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::identity::ReasoningEffort;
use orbit_types::telemetry::AgentExecutionIdentity;
use serde::Serialize;

use crate::Store;
use crate::contract::AgentExecutionBackfill;
use crate::repository::audit::v2::V2AuditRunEventRow;
use crate::repository::invocation::{AgentExecutionSlotRow, ExecutionSlotKey};

/// The audit event that records each provider launch and its rendered argv.
const CLI_INVOCATION_STARTED_EVENT: &str = "cli.invocation.started";

/// How far a launch's causal chain is followed before it is treated as
/// unproven; real chains are two links (launch, activity, step).
const CAUSAL_CHAIN_LIMIT: usize = 16;

/// How far a launch event may sit from an invocation's end minus its
/// duration. Launch and completion are stamped by different writers, and a
/// provider's own reported duration can trail the wall clock by several
/// seconds; a minute keeps every observed pairing while a retry launched in
/// the same run for the same model rarely falls inside it. When one does,
/// both launches must agree on the effort.
pub const LAUNCH_MATCH_TOLERANCE_SECONDS: i64 = 60;

/// [`LAUNCH_MATCH_TOLERANCE_SECONDS`] as a duration.
pub const LAUNCH_MATCH_TOLERANCE: chrono::Duration =
    chrono::Duration::seconds(LAUNCH_MATCH_TOLERANCE_SECONDS);

/// What one dispatch-effort backfill did in one workspace.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct DispatchEffortBackfill {
    pub workspace: String,
    /// Invocations with no dispatch identity when the backfill started.
    pub examined: usize,
    /// Invocations given a backfilled dispatch identity.
    pub recorded: usize,
    /// Recorded invocations by effort wire name, `provider_default` for a
    /// launch that rendered no effort flag.
    pub recorded_by_effort: BTreeMap<String, usize>,
    /// Invocations left unknown because no launch event matched them.
    pub no_launch_event: usize,
    /// Invocations left unknown because their matching launch events
    /// disagreed on the effort or carried an effort this binary cannot read.
    pub conflicting: usize,
    /// Invocations another writer gave an identity while this one ran.
    pub already_recorded: usize,
    /// Launch records recorded for runs whose invocation never completed:
    /// no invocation exists to carry an identity, so each launch is
    /// recorded on its own and the scorecard scan's launch-record fallback
    /// reads it.
    pub launch_only_recorded: usize,
    /// Launches left unrecorded because neither their causal links nor a
    /// single step interval of their run proved the step that ran them.
    pub launch_only_no_step: usize,
    /// Launches left unrecorded because the event named no provider.
    pub launch_only_no_provider: usize,
    /// Launches left unrecorded because that launch's recorded effort and
    /// argv disagreed, or its effort value could not be read. Another
    /// launch of the same step is recorded on its own.
    pub launch_only_unreadable: usize,
    /// [`Self::launch_only_no_step`] + [`Self::launch_only_no_provider`] +
    /// [`Self::launch_only_unreadable`].
    pub launch_only_unmatched: usize,
}

/// Completed invocation with no dispatch identity and its candidate launches.
struct UnidentifiedInvocationLaunch {
    invocation_id: i64,
    activity_id: String,
    run_resolved_crew: Option<String>,
    launches: Vec<InvocationLaunchEvent>,
}

/// One `cli.invocation.started` event that may have launched an invocation.
#[derive(Debug, Clone)]
struct InvocationLaunchEvent {
    event_id: String,
    started_at: DateTime<Utc>,
    argv_redacted: Option<Vec<String>>,
    reasoning_effort: Option<ReasoningEffort>,
    effort_unreadable: bool,
}

/// Launches of one run, step, provider and model that the dispatch-effort
/// backfill may still record, one row per launch.
struct UnrecordedLaunchGroup {
    run_id: String,
    activity_id: String,
    provider: String,
    model: Option<String>,
    run_resolved_crew: Option<String>,
    launches: Vec<InvocationLaunchEvent>,
}

#[derive(Default)]
struct UnrecordedLaunchOmissions {
    no_step: usize,
    no_provider: usize,
}

/// One step's audit interval in a run: `step.started` to its `step.finished`.
struct StepInterval {
    step_id: String,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
}

/// A run's `step.started` and `activity.started` audit events by event id:
/// the causal links from a launch to the step or run-level activity that ran
/// it.
struct CausalEvents {
    by_id: HashMap<String, CausalEvent>,
}

/// One causal link: its parent, and the step id (`step.started`) or
/// activity name (`activity.started`) it names.
struct CausalEvent {
    step: bool,
    parent: Option<String>,
    name: Option<String>,
}

impl CausalEvents {
    fn from_rows(rows: Vec<V2AuditRunEventRow>) -> Self {
        let by_id = rows
            .into_iter()
            .map(|row| {
                let step = row.event_type == "step.started";
                let payload = serde_json::from_str::<serde_json::Value>(&row.payload_json).ok();
                let name = payload
                    .as_ref()
                    .and_then(|payload| payload.get(if step { "step_id" } else { "activity_name" }))
                    .and_then(serde_json::Value::as_str)
                    .filter(|name| !name.trim().is_empty())
                    .map(str::to_string);
                (
                    row.event_id,
                    CausalEvent {
                        step,
                        parent: row.parent_event_id,
                        name,
                    },
                )
            })
            .collect();
        Self { by_id }
    }

    /// The step, else the run-level activity, a launch whose parent is
    /// `parent` ran under: the first `step.started` up its causal chain, or,
    /// when the chain ends at an `activity.started` with no parent, that
    /// activity's name. `None` when the chain is absent, broken, unnamed or
    /// too long: the launch is then not proven by its links.
    fn activity_of(&self, parent: Option<&str>) -> Option<&str> {
        let mut current = parent?;
        for _ in 0..CAUSAL_CHAIN_LIMIT {
            let event = self.by_id.get(current)?;
            if event.step {
                return event.name.as_deref();
            }
            match event.parent.as_deref() {
                Some(next) => current = next,
                None => return event.name.as_deref(),
            }
        }
        None
    }
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
            Some(raw) => match raw.parse::<ReasoningEffort>() {
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

/// Record a dispatch identity, read from the launch argv, for every
/// invocation of `workspace_id` that has none.
///
/// The crew is the run's resolved crew only for an invocation of one of
/// `executor_activities` (the run's executor); any other invocation's crew is
/// left absent rather than guessed.
pub fn backfill_dispatch_effort(
    store: &Store,
    workspace_id: &str,
    executor_activities: &[&str],
) -> Result<DispatchEffortBackfill, OrbitError> {
    let mut report = DispatchEffortBackfill {
        workspace: workspace_id.to_string(),
        ..DispatchEffortBackfill::default()
    };
    let recorded_at = chrono::Utc::now();
    for invocation in
        list_unidentified_invocation_launches(store, workspace_id, LAUNCH_MATCH_TOLERANCE)?
    {
        report.examined += 1;
        let Some(first) = invocation.launches.first() else {
            report.no_launch_event += 1;
            continue;
        };
        let Some(reasoning_effort) = agreed_launch_effort(&invocation.launches) else {
            report.conflicting += 1;
            continue;
        };
        let crew = if executor_activities.contains(&invocation.activity_id.as_str()) {
            invocation.run_resolved_crew.clone()
        } else {
            None
        };
        let backfill = AgentExecutionBackfill {
            source: CLI_INVOCATION_STARTED_EVENT.to_string(),
            event_ids: invocation
                .launches
                .iter()
                .map(|launch| launch.event_id.clone())
                .collect(),
            recorded_at,
        };
        let identity = AgentExecutionIdentity {
            crew,
            reasoning_effort,
        };
        let recorded = record_backfilled_agent_execution(
            store,
            workspace_id,
            invocation.invocation_id,
            first.started_at,
            &identity,
            &backfill,
        )?;
        if !recorded {
            report.already_recorded += 1;
            continue;
        }
        report.recorded += 1;
        let key =
            reasoning_effort.map_or_else(|| "provider_default".to_string(), |e| e.to_string());
        *report.recorded_by_effort.entry(key).or_default() += 1;
    }
    // Runs whose agent launched but whose invocation never completed have
    // no invocation to record an identity on. Record each readable launch
    // on its own, proven to its step by the run's audit trail, so the
    // scorecard scan's launch-record fallback can attribute a later round
    // of that step from that launch's time. A launch no single step
    // provably contained, that names no provider, or whose own effort
    // cannot be read, stays unknown and is never given an inferred effort.
    // Unreadable siblings stay in the set a dispatch-time row is matched
    // against: the closest one can own that row, and a later readable
    // launch is recorded from its own evidence. A launch a row already
    // covers is left as it is.
    let (launch_groups, omissions) = list_unrecorded_launch_groups(store, workspace_id)?;
    report.launch_only_no_step = omissions.no_step;
    report.launch_only_no_provider = omissions.no_provider;
    report.launch_only_unmatched = omissions.no_step + omissions.no_provider;
    for group in launch_groups {
        let crew = if executor_activities.contains(&group.activity_id.as_str()) {
            group.run_resolved_crew.clone()
        } else {
            None
        };
        let unreadable = group
            .launches
            .iter()
            .filter(|launch| launch_effort(launch).is_none())
            .count();
        report.launch_only_unreadable += unreadable;
        report.launch_only_unmatched += unreadable;
        for launch in group.launches.clone() {
            let Some(reasoning_effort) = launch_effort(&launch) else {
                continue;
            };
            let backfill = AgentExecutionBackfill {
                source: CLI_INVOCATION_STARTED_EVENT.to_string(),
                event_ids: vec![launch.event_id.clone()],
                recorded_at,
            };
            let identity = AgentExecutionIdentity {
                crew: crew.clone(),
                reasoning_effort,
            };
            let recorded = record_backfilled_launch_execution(
                store,
                workspace_id,
                &group,
                &launch,
                LAUNCH_MATCH_TOLERANCE,
                &identity,
                &backfill,
            )?;
            if recorded {
                report.launch_only_recorded += 1;
                let key = reasoning_effort
                    .map_or_else(|| "provider_default".to_string(), |e| e.to_string());
                *report.recorded_by_effort.entry(key).or_default() += 1;
            }
        }
    }
    Ok(report)
}

/// Match every unidentified invocation of `workspace_id` to candidate
/// `cli.invocation.started` events of the same run and model within `tolerance`
/// of `ended_at - duration_ms`.
fn list_unidentified_invocation_launches(
    store: &Store,
    workspace_id: &str,
    tolerance: chrono::Duration,
) -> Result<Vec<UnidentifiedInvocationLaunch>, OrbitError> {
    let snapshot = store.read()?;
    let rows = snapshot.invocation().list_unidentified(workspace_id)?;
    let v2 = snapshot.v2_audit();
    // Rows arrive grouped by run, so only one run's launches (whose argv
    // can be long) are held at a time.
    let mut run_launches: (Option<String>, Vec<RecordedLaunch>) = (None, Vec::new());
    let mut unidentified = Vec::with_capacity(rows.len());
    for row in rows {
        if run_launches.0.as_deref() != Some(row.run_id.as_str()) {
            let launches = v2
                .list_run_events_by_types(
                    workspace_id,
                    &row.run_id,
                    &[CLI_INVOCATION_STARTED_EVENT],
                )?
                .into_iter()
                .filter_map(|event| {
                    RecordedLaunch::parse(event.event_id, &event.ts, &event.payload_json)
                })
                .collect();
            run_launches = (Some(row.run_id.clone()), launches);
        }
        // A row whose end time does not parse cannot be placed in time,
        // so it gets no candidates and stays unknown.
        let launched_at = crate::parse_timestamp(&row.ended_at)
            .ok()
            .map(|ended| ended - chrono::Duration::milliseconds(row.duration_ms.max(0)));
        let launches = match launched_at {
            Some(launched_at) => run_launches
                .1
                .iter()
                .filter(|launch| {
                    launch.model == row.model
                        && (launch.event.started_at - launched_at).abs() <= tolerance
                })
                .map(|launch| launch.event.clone())
                .collect(),
            None => Vec::new(),
        };
        unidentified.push(UnidentifiedInvocationLaunch {
            invocation_id: row.id,
            activity_id: row.activity_id,
            run_resolved_crew: row.run_resolved_crew.filter(|crew| !crew.trim().is_empty()),
            launches,
        });
    }
    Ok(unidentified)
}

/// Atomically record `identity` for one invocation that still lacks an
/// `agent_execution_id`, returning `false` when already recorded or missing.
fn record_backfilled_agent_execution(
    store: &Store,
    workspace_id: &str,
    invocation_id: i64,
    launched_at: DateTime<Utc>,
    identity: &AgentExecutionIdentity,
    backfill: &AgentExecutionBackfill,
) -> Result<bool, OrbitError> {
    store.write(|tx| {
        let invocations = tx.invocation();
        let Some(target) = invocations.unidentified_target(workspace_id, invocation_id)? else {
            return Ok(false);
        };
        let execution_id = invocations.insert_agent_execution_with_backfill(
            workspace_id,
            &ExecutionSlotKey {
                run_id: &target.run_id,
                activity_id: &target.activity_id,
                provider: &target.provider,
                model: target.model.as_deref(),
            },
            launched_at,
            identity,
            backfill,
        )?;
        invocations.link_agent_execution(invocation_id, execution_id)?;
        Ok(true)
    })
}

/// Group launch events of `workspace_id` runs by their proven step, provider
/// and model, counting launches whose step or provider cannot be proven.
fn list_unrecorded_launch_groups(
    store: &Store,
    workspace_id: &str,
) -> Result<(Vec<UnrecordedLaunchGroup>, UnrecordedLaunchOmissions), OrbitError> {
    let snapshot = store.read()?;
    let crews = snapshot.invocation().list_job_run_crews(workspace_id)?;
    let v2 = snapshot.v2_audit();
    let run_ids = v2.run_ids_for_event_type(workspace_id, CLI_INVOCATION_STARTED_EVENT)?;
    let mut groups: Vec<UnrecordedLaunchGroup> = Vec::new();
    let mut omissions = UnrecordedLaunchOmissions::default();
    for run_id in run_ids {
        let Some(crew) = crews.get(&run_id) else {
            continue;
        };
        let launches: Vec<(RecordedLaunch, Option<String>)> = v2
            .list_run_events_by_types(workspace_id, &run_id, &[CLI_INVOCATION_STARTED_EVENT])?
            .into_iter()
            .filter_map(|event| {
                RecordedLaunch::parse(event.event_id, &event.ts, &event.payload_json)
                    .map(|launch| (launch, event.parent_event_id))
            })
            .collect();
        if launches.is_empty() {
            continue;
        }
        let cause_rows = v2.list_run_events_by_types(
            workspace_id,
            &run_id,
            &["step.started", "activity.started"],
        )?;
        let causes = CausalEvents::from_rows(cause_rows);
        let step_rows =
            v2.list_run_events_by_types(workspace_id, &run_id, &["step.started", "step.finished"])?;
        let intervals = step_intervals_from_rows(step_rows);
        for (launch, parent) in launches {
            let step_id = match causes.activity_of(parent.as_deref()) {
                Some(activity) => activity.to_string(),
                None => {
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
                    only.step_id.clone()
                }
            };
            let Some(provider) = launch.provider.clone() else {
                omissions.no_provider += 1;
                continue;
            };
            let key = (&step_id, &provider, &launch.model);
            if let Some(group) = groups.iter_mut().find(|group| {
                group.run_id == run_id
                    && group.activity_id == *key.0
                    && group.provider == *key.1
                    && group.model == *key.2
            }) {
                group.launches.push(launch.event);
            } else {
                groups.push(UnrecordedLaunchGroup {
                    run_id: run_id.clone(),
                    activity_id: step_id.clone(),
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

/// Atomically record one launch-only execution if no existing row for its slot
/// already covers `launch`.
fn record_backfilled_launch_execution(
    store: &Store,
    workspace_id: &str,
    group: &UnrecordedLaunchGroup,
    launch: &InvocationLaunchEvent,
    tolerance: chrono::Duration,
    identity: &AgentExecutionIdentity,
    backfill: &AgentExecutionBackfill,
) -> Result<bool, OrbitError> {
    store.write(|tx| {
        let invocations = tx.invocation();
        let existing = invocations.agent_executions_for_slot(
            workspace_id,
            &group.run_id,
            &group.activity_id,
            &group.provider,
            group.model.as_deref(),
        )?;
        for row in &existing {
            if launch_row_covers(row, launch, &group.launches, tolerance) {
                return Ok(false);
            }
        }
        invocations.insert_agent_execution_with_backfill(
            workspace_id,
            &ExecutionSlotKey {
                run_id: &group.run_id,
                activity_id: &group.activity_id,
                provider: &group.provider,
                model: group.model.as_deref(),
            },
            launch.started_at,
            identity,
            backfill,
        )?;
        Ok(true)
    })
}

/// Whether one stored row already covers `launch`.
fn launch_row_covers(
    row: &AgentExecutionSlotRow,
    launch: &InvocationLaunchEvent,
    siblings: &[InvocationLaunchEvent],
    tolerance: chrono::Duration,
) -> bool {
    if let Some(backfill) = &row.backfill {
        return backfill_row_covers_launch(backfill, row.started_at, launch);
    }
    dispatch_row_covers_launch(row.started_at, launch, siblings, tolerance)
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

fn step_intervals_from_rows(rows: Vec<V2AuditRunEventRow>) -> Vec<StepInterval> {
    let mut intervals: Vec<StepInterval> = Vec::new();
    for row in rows {
        let Ok(at) = crate::parse_timestamp(&row.ts) else {
            continue;
        };
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(&row.payload_json) else {
            continue;
        };
        let Some(step_id) = payload
            .get("step_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        if row.event_type == "step.started" {
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
    intervals
}

/// The one effort every launch rendered (`Some(None)` for the provider
/// default), or `None` when they disagree or one is unreadable.
fn agreed_launch_effort(launches: &[InvocationLaunchEvent]) -> Option<Option<ReasoningEffort>> {
    let efforts = launches
        .iter()
        .map(launch_effort)
        .collect::<Option<BTreeSet<_>>>()?;
    match efforts.len() {
        1 => efforts.into_iter().next(),
        _ => None,
    }
}

/// One launch's effort: the effort the event itself records, the effort its
/// argv rendered, or `None` when it carries neither readable source or the
/// two disagree. An event with no argv and no recorded effort is no
/// evidence at all, never the provider default.
fn launch_effort(launch: &InvocationLaunchEvent) -> Option<Option<ReasoningEffort>> {
    if launch.effort_unreadable {
        return None;
    }
    let argv_effort = match launch.argv_redacted.as_deref() {
        Some(argv) => Some(rendered_effort(argv)?),
        None => None,
    };
    match (launch.reasoning_effort, argv_effort) {
        (Some(recorded), Some(rendered)) if Some(recorded) != rendered => None,
        (Some(recorded), _) => Some(Some(recorded)),
        (None, Some(rendered)) => Some(rendered),
        (None, None) => None,
    }
}

/// The effort a provider argv rendered, in the flag forms Orbit's provider
/// transports write: `--effort X` (Claude, Antigravity), `--reasoning-effort X`
/// (Grok, Muse) and `model_reasoning_effort="X"` (a Codex `--config` value).
/// `Some(None)` when the argv carries no effort flag (the provider default);
/// `None` when a flag's value is not an effort or two flags disagree.
fn rendered_effort(argv: &[String]) -> Option<Option<ReasoningEffort>> {
    let mut values = Vec::new();
    let mut args = argv.iter();
    while let Some(arg) = args.next() {
        if arg == "--effort" || arg == "--reasoning-effort" {
            values.push(args.next()?.as_str());
        } else if let Some(value) = arg.strip_prefix("model_reasoning_effort=") {
            values.push(value.trim_matches('"'));
        }
    }
    let efforts = values
        .into_iter()
        .map(|value| value.parse::<ReasoningEffort>().ok())
        .collect::<Option<BTreeSet<_>>>()?;
    match efforts.len() {
        0 => Some(None),
        1 => efforts.into_iter().next().map(Some),
        _ => None,
    }
}
