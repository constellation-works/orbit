//! The shadow share report: what recorded crew draws would have done with
//! the performance factor they stored but, under a shadow mode, did not use.
//!
//! Draws are the implementation job runs in the window. The coordinator's
//! `task_auto_pipeline` is left out: its children inherit the same
//! `crew_selection`, and counting both would count one draw twice. The four
//! job names match `facts::IMPLEMENTATION_JOBS`; that list stays private so
//! this copy is the one to keep in step with it.

use std::collections::BTreeMap;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use serde_json::{Value, json};

use super::AgentQuery;
use crate::OrbitRuntime;

/// The jobs whose runs implement a task. Keep this in step with
/// `facts::IMPLEMENTATION_JOBS`.
const IMPLEMENTATION_JOBS: &[&str] = &[
    "task_pr_pipeline",
    "task_local_pipeline",
    "task_claimed_pr_pipeline",
    "task_claimed_local_pipeline",
];

/// One crew's summed per-draw shares and the times it was selected.
struct CrewShares {
    draws: u64,
    static_share: f64,
    drawn_share: f64,
    would_share: f64,
}

/// One pool's draws, split by whether a performance object was recorded.
struct PoolShares {
    draws: u64,
    crews: BTreeMap<String, CrewShares>,
}

/// The shadow report for `query`. `min_support` is ignored: every recorded
/// draw in the window counts, including a crew selected once.
pub(super) fn report(runtime: &OrbitRuntime, query: &AgentQuery) -> Result<Value, OrbitError> {
    let since = query.since();
    let selections = recorded_selections(runtime, since)?;
    let mut recorded: BTreeMap<(String, String), PoolShares> = BTreeMap::new();
    let mut static_pools: BTreeMap<String, PoolShares> = BTreeMap::new();
    let mut static_draws = 0_u64;
    for selection in selections {
        let Some(draw) = parse_draw(&selection) else {
            continue;
        };
        let bucket = if draw.recorded {
            recorded
                .entry((draw.pool.clone(), draw.task_type.clone()))
                .or_insert_with(PoolShares::new)
        } else {
            static_draws += 1;
            static_pools
                .entry(draw.pool.clone())
                .or_insert_with(PoolShares::new)
        };
        bucket.add(&draw);
    }
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    Ok(json!({
        "role": "implementers",
        "by_type": true,
        "shadow": true,
        "window": query.window,
        "requested_window": query.requested,
        "since": since.to_rfc3339_opts(SecondsFormat::Secs, true),
        "computed_at": now,
        "pools": recorded
            .into_iter()
            .map(|((pool, task_type), shares)| shares.recorded_json(&pool, &task_type))
            .collect::<Vec<_>>(),
        "static_draws": {
            "draws": static_draws,
            "pools": static_pools
                .into_iter()
                .map(|(pool, shares)| shares.static_json(&pool))
                .collect::<Vec<_>>(),
        },
    }))
}

fn recorded_selections(
    runtime: &OrbitRuntime,
    since: DateTime<Utc>,
) -> Result<Vec<Value>, OrbitError> {
    let jobs = runtime.stores().jobs();
    let mut selections = Vec::new();
    for job in IMPLEMENTATION_JOBS {
        let runs = jobs.list_job_runs_filtered(&JobRunQuery {
            job_id: Some((*job).to_string()),
            recency_since: Some(since),
            include_steps: false,
            ..JobRunQuery::default()
        })?;
        for run in runs {
            // `recency_since` keeps a run that finished inside the window
            // even when it started before it. The draw is the start.
            let draw_at = run.started_at.unwrap_or(run.created_at);
            if draw_at < since {
                continue;
            }
            if let Some(selection) = run
                .input
                .as_ref()
                .and_then(|input| input.get("crew_selection"))
                .filter(|selection| selection.is_object())
            {
                selections.push(selection.clone());
            }
        }
    }
    Ok(selections)
}

/// One draw reduced to the weights the share math needs.
struct Draw {
    pool: String,
    task_type: String,
    selected: String,
    recorded: bool,
    members: Vec<Member>,
}

struct Member {
    crew: String,
    static_w: f64,
    drawn_w: f64,
    would_w: f64,
}

fn parse_draw(selection: &Value) -> Option<Draw> {
    let pool = selection.get("source")?.as_str()?.split(';').next()?.trim();
    if pool.is_empty() {
        return None;
    }
    let selected = selection.get("crew")?.as_str()?.to_string();
    let eligible = selection.get("eligible_pool")?.as_array()?;
    if eligible.is_empty() {
        return None;
    }
    let mut members = Vec::new();
    let mut recorded = false;
    let mut task_type = None;
    for entry in eligible {
        let Some(member) = parse_member(entry, &mut task_type, &mut recorded) else {
            continue;
        };
        members.push(member);
    }
    if members.is_empty() {
        return None;
    }
    let static_total: f64 = members.iter().map(|member| member.static_w).sum();
    if static_total <= 0.0 {
        return None;
    }
    // A draw some of whose members recorded a factor is a recorded draw.
    // A member with no object counts as factor 1, applied, so its would-be
    // share is the weight it drew on.
    Some(Draw {
        pool: pool.to_string(),
        task_type: task_type.unwrap_or_else(|| "unknown".to_string()),
        selected,
        recorded,
        members,
    })
}

fn parse_member(
    entry: &Value,
    task_type: &mut Option<String>,
    recorded: &mut bool,
) -> Option<Member> {
    let crew = entry.get("name")?.as_str()?.to_string();
    let static_w = finite(entry.get("weight")?.as_f64()?)?;
    if static_w < 0.0 {
        return None;
    }
    let drawn_w = entry
        .get("effective_weight")
        .and_then(Value::as_f64)
        .and_then(finite)
        .unwrap_or(static_w);
    let Some(performance) = entry.get("performance").filter(|value| value.is_object()) else {
        return Some(Member {
            crew,
            static_w,
            drawn_w,
            would_w: drawn_w,
        });
    };
    *recorded = true;
    if task_type.is_none() {
        *task_type = performance
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    let factor = performance
        .get("factor")
        .and_then(Value::as_f64)
        .and_then(finite)
        .unwrap_or(1.0);
    // Missing `applied` is a record from before shadow mode: the factor is
    // already inside `effective_weight` when the draw used it.
    let applied = performance
        .get("applied")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let would_w = if applied { drawn_w } else { drawn_w * factor };
    Some(Member {
        crew,
        static_w,
        drawn_w,
        would_w,
    })
}

fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

impl PoolShares {
    fn new() -> Self {
        Self {
            draws: 0,
            crews: BTreeMap::new(),
        }
    }

    fn add(&mut self, draw: &Draw) {
        self.draws += 1;
        let static_total: f64 = draw.members.iter().map(|member| member.static_w).sum();
        let drawn_total: f64 = draw.members.iter().map(|member| member.drawn_w).sum();
        let would_total: f64 = draw.members.iter().map(|member| member.would_w).sum();
        for member in &draw.members {
            let shares = self
                .crews
                .entry(member.crew.clone())
                .or_insert_with(CrewShares::new);
            if member.crew == draw.selected {
                shares.draws += 1;
            }
            shares.static_share += portion(member.static_w, static_total);
            shares.drawn_share += portion(member.drawn_w, drawn_total);
            shares.would_share += portion(member.would_w, would_total);
        }
    }

    fn recorded_json(&self, pool: &str, task_type: &str) -> Value {
        json!({
            "pool": pool,
            "type": task_type,
            "draws": self.draws,
            "crews": self.crews.iter().map(|(crew, shares)| json!({
                "crew": crew,
                "draws": shares.draws,
                "static_share": self.mean(shares.static_share),
                "drawn_share": self.mean(shares.drawn_share),
                "would_be_share": self.mean(shares.would_share),
            })).collect::<Vec<_>>(),
        })
    }

    /// The mean per-draw share, rounded to four decimal places. A crew
    /// absent from a draw added nothing, so this is the mean over every
    /// draw in the group.
    fn mean(&self, sum: f64) -> f64 {
        if self.draws == 0 {
            0.0
        } else {
            round4(sum / self.draws as f64)
        }
    }

    fn static_json(&self, pool: &str) -> Value {
        json!({
            "pool": pool,
            "draws": self.draws,
            "crews": self.crews.iter().map(|(crew, shares)| json!({
                "crew": crew,
                "draws": shares.draws,
                "static_share": self.mean(shares.static_share),
                "drawn_share": self.mean(shares.drawn_share),
            })).collect::<Vec<_>>(),
        })
    }
}

impl CrewShares {
    fn new() -> Self {
        Self {
            draws: 0,
            static_share: 0.0,
            drawn_share: 0.0,
            would_share: 0.0,
        }
    }
}

/// A crew absent from a draw contributes nothing, so dividing the summed
/// shares by the group's draw count is the mean over every draw in it.
fn portion(part: f64, total: f64) -> f64 {
    if total > 0.0 { part / total } else { 0.0 }
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}
