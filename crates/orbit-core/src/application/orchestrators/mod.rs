//! Orchestrator scorecard: ranks the orchestrators named by tasks'
//! `orchestrator` attribution (distinct from the execution crew) by the work
//! they file and supervise. `orbit orchestrator` and the
//! `orbit.orchestrator.*` tools read this one service; the dashboard ranks
//! orchestrators through the agent scorecards (`application::agents`).
//!
//! Per orchestrator, over the tasks it owns that were created in the window:
//! throughput, proactivity from the typed origin, spec defects and rework,
//! each normalized per 100 owned tasks and per 8 hours of active time, then a
//! composite whose weights live in one table. Every count is read from task
//! history, comments, relations and pull request references; orchestrator
//! session cost is not captured reliably and is not measured.
//!
//! [`OrbitRuntime::orchestrator_scan`] reads every attributed task's history
//! once; [`orchestrator_scorecard`] and [`orchestrator_detail`] only slice
//! those facts by window and minimum support, so a caller that keeps the
//! facts (the dashboard) answers another selection without rescanning.

mod facts;
mod scorecard;

use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::OrbitRuntime;

pub use facts::OrchestratorFacts;

const DEFAULT_WINDOW: &str = "30d";
const DEFAULT_MIN_SUPPORT: usize = 10;
/// The selectable windows and their length in hours; `all` is unbounded.
const WINDOWS: &[(&str, Option<i64>)] = &[
    ("1h", Some(1)),
    ("24h", Some(24)),
    ("7d", Some(7 * 24)),
    ("30d", Some(30 * 24)),
    ("90d", Some(90 * 24)),
    ("all", None),
];

/// A validated selection: the window and the owned tasks a row needs before
/// its rates are read as comparable.
#[derive(Debug, Clone, Copy)]
pub struct OrchestratorQuery {
    window: &'static str,
    hours: Option<i64>,
    min_support: usize,
}

impl OrchestratorQuery {
    /// Validate a selection. An omitted window is `30d` and an omitted
    /// minimum support 10.
    pub fn parse(window: Option<&str>, min_support: Option<usize>) -> Result<Self, OrbitError> {
        let window = window.unwrap_or(DEFAULT_WINDOW);
        let Some(&(window, hours)) = WINDOWS.iter().find(|(name, _)| *name == window) else {
            return Err(OrbitError::InvalidInput(format!(
                "window must be one of 1h, 24h, 7d, 30d, 90d or all; got '{window}'"
            )));
        };
        let min_support = min_support.unwrap_or(DEFAULT_MIN_SUPPORT);
        if min_support == 0 {
            return Err(OrbitError::InvalidInput(
                "min_support must be at least 1".to_string(),
            ));
        }
        Ok(Self {
            window,
            hours,
            min_support,
        })
    }

    /// The window's start; it ends now.
    fn since(&self) -> Option<DateTime<Utc>> {
        self.hours
            .map(|hours| Utc::now() - chrono::Duration::hours(hours))
    }

    /// The selection, scan facts and definitions every response carries.
    fn header(&self, facts: &OrchestratorFacts, since: Option<DateTime<Utc>>) -> Value {
        let in_window = |created: &DateTime<Utc>| since.is_none_or(|since| *created >= since);
        json!({
            "window": self.window,
            "since": since.map(|since| since.to_rfc3339_opts(SecondsFormat::Secs, true)),
            "min_support": self.min_support,
            "computed_at": facts.computed_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            "tasks_scanned": facts.tasks_scanned,
            "attributed_tasks": facts.tasks.iter().filter(|task| in_window(&task.created_at)).count(),
            "unattributed_tasks": facts.unattributed.iter().filter(|created| in_window(created)).count(),
            "definitions": scorecard::definitions(),
        })
    }
}

impl OrbitRuntime {
    /// Scan every task an orchestrator is attributed to once. A task whose
    /// history cannot be read keeps its envelope facts.
    pub fn orchestrator_scan(&self) -> Result<OrchestratorFacts, OrbitError> {
        facts::scan(self)
    }

    /// Scan, then score for `query`. For callers that keep no scan, so the
    /// response reports a cache period of zero.
    pub fn orchestrator_scorecard(&self, query: &OrchestratorQuery) -> Result<Value, OrbitError> {
        Ok(orchestrator_scorecard(
            &self.orchestrator_scan()?,
            query,
            Duration::ZERO,
        ))
    }

    /// Scan, then report one orchestrator's row with the tasks behind each
    /// count.
    pub fn orchestrator_detail(
        &self,
        query: &OrchestratorQuery,
        orchestrator: &str,
    ) -> Result<Value, OrbitError> {
        orchestrator_detail(&self.orchestrator_scan()?, query, orchestrator)
    }
}

/// One row per orchestrator with an owned task in the window, with the
/// selection, scan facts and definitions. `cache_ttl` is how long the caller
/// keeps `facts`, reported as `cache_ttl_seconds`.
pub fn orchestrator_scorecard(
    facts: &OrchestratorFacts,
    query: &OrchestratorQuery,
    cache_ttl: Duration,
) -> Value {
    let since = query.since();
    let mut payload = query.header(facts, since);
    payload["rows"] = scorecard::score(facts, since, query.min_support)
        .into_iter()
        .map(|scored| scored.row)
        .collect();
    payload["cache_ttl_seconds"] = json!(cache_ttl.as_secs());
    payload
}

/// One orchestrator's row and, for every component, the owned tasks behind
/// its count with the records that make it. An orchestrator that owns no
/// task in the window is invalid input.
pub fn orchestrator_detail(
    facts: &OrchestratorFacts,
    query: &OrchestratorQuery,
    orchestrator: &str,
) -> Result<Value, OrbitError> {
    let since = query.since();
    let name = orchestrator.trim();
    let Some(scored) = scorecard::score(facts, since, query.min_support)
        .into_iter()
        .find(|scored| scored.name == name)
    else {
        return Err(OrbitError::InvalidInput(format!(
            "no task created in the {} window names orchestrator '{name}'",
            query.window
        )));
    };
    let mut payload = query.header(facts, since);
    payload["orchestrator"] = json!(scored.name);
    payload["components"] = json!(scorecard::drill_down(&scored.tasks));
    payload["row"] = scored.row;
    Ok(payload)
}
