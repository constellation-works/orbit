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
//! one step whose audit interval contains it. A later launch of the same
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
//! cannot read. A launch stays unrecorded when no single step contains it,
//! when it names no provider, or when that launch's own effort cannot be
//! read; another launch of the same step is recorded on its own.
//! `cli.invocation.activity` events carry no argv or effort and are never a
//! source. Effort is never taken from crew configuration or a crew name.
//! Invocations that already have an identity are never touched, so running
//! it again changes nothing.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_types::identity::ReasoningEffort;
use orbit_types::telemetry::AgentExecutionIdentity;
use serde::Serialize;

use crate::Store;
use crate::contracts::{AgentExecutionBackfill, InvocationLaunchEvent};
use crate::driver::sqlite::invocation_store::CLI_INVOCATION_STARTED_EVENT;

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
    /// Launches left unrecorded because no single step of their run provably
    /// contained them.
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
        store.list_unidentified_invocation_launches(workspace_id, LAUNCH_MATCH_TOLERANCE)?
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
        let recorded = store.record_backfilled_agent_execution(
            workspace_id,
            invocation.invocation_id,
            first.started_at,
            &AgentExecutionIdentity {
                crew,
                reasoning_effort,
            },
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
    let (launch_groups, omissions) = store.list_unrecorded_launch_groups(workspace_id)?;
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
            let recorded = store.record_backfilled_launch_execution(
                workspace_id,
                &group,
                &launch,
                LAUNCH_MATCH_TOLERANCE,
                &AgentExecutionIdentity {
                    crew: crew.clone(),
                    reasoning_effort,
                },
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
