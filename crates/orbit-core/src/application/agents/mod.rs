//! Agent scorecards: one scorecard per agent role (orchestrators,
//! implementers, reviewers, pilots), ranked by crew and model, where every
//! problem counts against exactly one role or is Not scored.
//!
//! [`OrbitRuntime::agent_scan`] reads the workspace once ([`AgentFacts`]):
//! tasks with activity in the last 90 days, the implementation job runs that
//! name them, the review gate's settlements, the verdicts on review-filed
//! findings and the typed regressions, and gives each problem its one owner
//! ([`Charge`]). [`agent_scorecard`],
//! [`agent_detail`] and [`agent_attribution`] only slice those facts by
//! role, window and minimum support, so a caller that keeps the facts (the
//! dashboard) answers another selection without rescanning.
//!
//! Every role shares one score: each term's count per 100 units of the
//! role's work (or of the term's own denominator, as the pilots' warning
//! terms are rated per 100 settled assessments), weighted and summed, then,
//! above the fleet average, pulled down toward it with K = 30. Cost is never
//! a term.

mod attribution;
mod candidates;
mod facts;
mod records;
mod roles;
mod score;
mod shadow;

use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::OrbitRuntime;

pub use attribution::{Charge, Credit, Owner};
pub use facts::{AgentFacts, Problem};

/// The model of a row whose runs, or for orchestrators whose tasks, recorded
/// none.
pub(crate) const MODEL_UNKNOWN: &str = "model unknown";
/// The crew of a row whose attempt no job run records.
pub(crate) const CREW_UNKNOWN: &str = "crew unknown";

const DEFAULT_WINDOW: &str = "30d";
const DEFAULT_MIN_SUPPORT: usize = 10;
/// The selectable windows: requested name, the window served and its hours.
/// `all` is served as 90 days, the scan's reach.
const WINDOWS: &[(&str, &str, i64)] = &[
    ("1h", "1h", 1),
    ("24h", "24h", 24),
    ("7d", "7d", 7 * 24),
    ("30d", "30d", 30 * 24),
    ("90d", "90d", facts::SCAN_DAYS * 24),
    ("all", "90d", facts::SCAN_DAYS * 24),
];

/// An agent role, each with its own scorecard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AgentRole {
    Orchestrators,
    Implementers,
    Reviewers,
    Pilots,
}

impl AgentRole {
    pub const ALL: [AgentRole; 4] = [
        AgentRole::Orchestrators,
        AgentRole::Implementers,
        AgentRole::Reviewers,
        AgentRole::Pilots,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AgentRole::Orchestrators => "orchestrators",
            AgentRole::Implementers => "implementers",
            AgentRole::Reviewers => "reviewers",
            AgentRole::Pilots => "pilots",
        }
    }

    /// A role by its wire name.
    pub fn parse(name: &str) -> Result<Self, OrbitError> {
        Self::ALL
            .into_iter()
            .find(|role| role.as_str() == name.trim())
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "role must be one of orchestrators, implementers, reviewers or pilots; got '{name}'"
                ))
            })
    }
}

/// A validated selection: the window and the units a row needs before it is
/// ranked.
#[derive(Debug, Clone, Copy)]
pub struct AgentQuery {
    requested: &'static str,
    window: &'static str,
    hours: i64,
    min_support: usize,
}

impl AgentQuery {
    /// Validate a selection. An omitted window is `30d` and an omitted
    /// minimum support 10.
    pub fn parse(window: Option<&str>, min_support: Option<usize>) -> Result<Self, OrbitError> {
        let window = window.unwrap_or(DEFAULT_WINDOW);
        let Some(&(requested, served, hours)) = WINDOWS.iter().find(|(name, _, _)| *name == window)
        else {
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
            requested,
            window: served,
            hours,
            min_support,
        })
    }

    /// The window's start; it ends now.
    fn since(&self) -> DateTime<Utc> {
        Utc::now() - chrono::Duration::hours(self.hours)
    }

    /// The selection and scan facts every response carries.
    fn header(&self, facts: &AgentFacts, since: DateTime<Utc>, cache_ttl: Duration) -> Value {
        json!({
            "window": self.window,
            "requested_window": self.requested,
            "since": since.to_rfc3339_opts(SecondsFormat::Secs, true),
            "min_support": self.min_support,
            "computed_at": facts.computed_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            "cache_ttl_seconds": cache_ttl.as_secs(),
            "tasks_scanned": facts.tasks_scanned,
        })
    }
}

impl OrbitRuntime {
    /// Scan the workspace once for every agent scorecard. A task whose
    /// history cannot be read keeps its envelope facts.
    pub fn agent_scan(&self) -> Result<AgentFacts, OrbitError> {
        facts::scan(self)
    }

    /// Scan, then rank one role for `query`. For callers that keep no scan
    /// (the CLI and the `orbit.agents.*` tools), so the response reports a
    /// cache period of zero.
    pub fn agent_role_scorecard(
        &self,
        role: AgentRole,
        query: &AgentQuery,
    ) -> Result<Value, OrbitError> {
        Ok(agent_scorecard(
            &self.agent_scan()?,
            role,
            query,
            Duration::ZERO,
        ))
    }

    /// Scan, then report the implementer by-type cell matrix, with a cache
    /// period of zero as on [`Self::agent_role_scorecard`].
    pub fn agent_by_type_scorecard(&self, query: &AgentQuery) -> Result<Value, OrbitError> {
        Ok(agent_by_type_scorecard(
            &self.agent_scan()?,
            query,
            self.crew_performance_types(),
            Duration::ZERO,
        ))
    }

    /// The implementer shadow share report for `query`: each crew's static,
    /// drawn and would-be share per pool and type, from recorded draws in
    /// the window. This does not scan tasks.
    pub fn agent_shadow_scorecard(&self, query: &AgentQuery) -> Result<Value, OrbitError> {
        shadow::report(self, query)
    }

    /// Rebuild this workspace's crew-performance snapshot from a fresh scan
    /// over `workflow.crew_performance_window_days`, replacing the stored
    /// one atomically. With `workflow.crew_performance` off this does not
    /// scan and writes no snapshot in workspaces that do not use the
    /// feature (the hourly routine still records a job run).
    pub fn refresh_crew_performance_snapshot(&self) -> Result<Value, OrbitError> {
        if !self.crew_performance().is_on() {
            return Ok(json!({ "skipped": "crew_performance_off" }));
        }
        let window_days = self.crew_performance_window_days();
        let types = self.crew_performance_types();
        let facts = self.agent_scan()?;
        let since = Utc::now() - chrono::Duration::days(i64::from(window_days));
        let cells = roles::implementer_by_type_cells(&facts, since, types)
            .iter()
            .map(crew_performance_cell)
            .collect::<Result<Vec<_>, OrbitError>>()?;
        let snapshot = orbit_store::contracts::CrewPerformanceSnapshot {
            workspace_id: self.workspace_id()?,
            computed_at: facts.computed_at,
            window_days,
            types_hash: orbit_config::crew_performance_types_hash(types),
            types_json: serde_json::to_string(types).map_err(|error| {
                OrbitError::Execution(format!("serialize crew performance types: {error}"))
            })?,
            cells,
        };
        let cell_count = snapshot.cells.len();
        let computed_at = snapshot
            .computed_at
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let types_hash = snapshot.types_hash.clone();
        use orbit_store::contracts::CrewPerformanceStoreBackend as _;
        self.sqlite_store()?
            .replace_crew_performance_snapshot(&snapshot)?;
        Ok(json!({
            "computed_at": computed_at,
            "window_days": window_days,
            "types_hash": types_hash,
            "cells": cell_count,
        }))
    }

    /// This workspace's stored crew-performance snapshot when a draw may
    /// use it (fresh, built from the current type list), else the typed
    /// reason it may not.
    pub fn usable_crew_performance_snapshot(
        &self,
    ) -> Result<
        Result<
            orbit_store::contracts::CrewPerformanceSnapshot,
            orbit_store::contracts::CrewPerformanceSnapshotUnusable,
        >,
        OrbitError,
    > {
        use orbit_store::contracts::CrewPerformanceStoreBackend as _;
        self.sqlite_store()?.usable_crew_performance_snapshot(
            &self.workspace_id()?,
            Utc::now(),
            &orbit_config::crew_performance_types_hash(self.crew_performance_types()),
        )
    }

    /// The readiness view of the snapshot: its metadata when one is stored,
    /// plus the unusable reason when a draw would ignore it. Advisory, so a
    /// store that cannot be read reports `unreadable` beside the error
    /// rather than failing readiness, so it is not confused with `missing`.
    pub fn crew_performance_readiness(&self) -> Value {
        let mode = self.crew_performance().as_str();
        let stored = (|runtime: &Self| -> Result<_, OrbitError> {
            use orbit_store::contracts::CrewPerformanceStoreBackend as _;
            runtime
                .sqlite_store()?
                .crew_performance_snapshot(&runtime.workspace_id()?)
        })(self);
        match stored {
            Ok(snapshot) => {
                let types_hash =
                    orbit_config::crew_performance_types_hash(self.crew_performance_types());
                let unusable = orbit_store::contracts::usable_crew_performance_snapshot(
                    snapshot.clone(),
                    Utc::now(),
                    &types_hash,
                )
                .err();
                json!({
                    "mode": mode,
                    "snapshot": snapshot.map(|snapshot| json!({
                        "computed_at": snapshot.computed_at
                            .to_rfc3339_opts(SecondsFormat::Secs, true),
                        "window_days": snapshot.window_days,
                        "types": serde_json::from_str::<Value>(&snapshot.types_json)
                            .unwrap_or(Value::Null),
                        "types_hash": snapshot.types_hash,
                        "cells": snapshot.cells.len(),
                    })),
                    "unusable_reason": unusable.map(|reason| reason.as_str()),
                })
            }
            Err(error) => json!({
                "mode": mode,
                "snapshot": Value::Null,
                "unusable_reason": "unreadable",
                "error": error.to_string(),
            }),
        }
    }

    /// Scan, then report one row of a role with the tasks behind each term,
    /// with a cache period of zero as on [`Self::agent_role_scorecard`].
    pub fn agent_role_detail(
        &self,
        role: AgentRole,
        query: &AgentQuery,
        crew: &str,
        model: Option<&str>,
    ) -> Result<Value, OrbitError> {
        agent_detail(
            &self.agent_scan()?,
            role,
            query,
            crew,
            model,
            Duration::ZERO,
        )
    }
}

/// A role's scorecard header: selection, unit, terms and status.
fn role_header(
    facts: &AgentFacts,
    role: AgentRole,
    query: &AgentQuery,
    since: DateTime<Utc>,
    cache_ttl: Duration,
) -> Value {
    let spec = roles::spec(role);
    let mut payload = query.header(facts, since, cache_ttl);
    payload["role"] = json!(role.as_str());
    payload["unit"] = json!(spec.unit);
    payload["status"] = json!(if spec.rows.is_some() {
        "ok"
    } else {
        "not_computed"
    });
    payload["terms"] = json!(score::terms_json(spec.terms, spec.term_units));
    if let Some(header) = spec.header {
        for (key, value) in header(facts, since) {
            payload[key] = value;
        }
    }
    payload
}

/// One role's ranking for `query`: one row per crew and model, best first,
/// rows below the minimum support last without a rank. A role not computed
/// yet answers `not_computed` with its terms and no rows. `cache_ttl` is how
/// long the caller keeps `facts`, reported as `cache_ttl_seconds`.
pub fn agent_scorecard(
    facts: &AgentFacts,
    role: AgentRole,
    query: &AgentQuery,
    cache_ttl: Duration,
) -> Value {
    let since = query.since();
    let spec = roles::spec(role);
    let mut payload = role_header(facts, role, query, since, cache_ttl);
    let (fleet_average, scored) = match spec.rows {
        Some(rows) => score::score(spec.terms, rows(facts, since), query.min_support),
        None => (None, Vec::new()),
    };
    payload["fleet_average"] = json!(fleet_average.map(score::number));
    payload["provisional"] = json!(score::provisional(role, &scored));
    payload["rows"] = scored.iter().map(|row| row.row(spec.terms)).collect();
    payload
}

/// The implementer cell matrix for `query`: one cell per `(crew, model,
/// tier, type)` with its `n`, `raw`, `crew_mean` and shrunk `score`, plus
/// the type list the tasks were classified with.
pub fn agent_by_type_scorecard(
    facts: &AgentFacts,
    query: &AgentQuery,
    types: &[orbit_config::CrewPerformanceType],
    cache_ttl: Duration,
) -> Value {
    let since = query.since();
    let role = AgentRole::Implementers;
    let mut payload = role_header(facts, role, query, since, cache_ttl);
    payload["by_type"] = json!(true);
    payload["types"] = json!(
        types
            .iter()
            .map(|entry| json!({
                "name": entry.name,
                "tags": entry.tags,
                "paths": entry.paths,
            }))
            .collect::<Vec<_>>()
    );
    payload["cells"] = json!(roles::implementer_by_type_cells(facts, since, types));
    payload
}

/// One by-type cell JSON object as the persisted snapshot cell, so a stored
/// row is byte-for-byte the scorecard's cell for the same window and types.
fn crew_performance_cell(
    cell: &Value,
) -> Result<orbit_store::contracts::CrewPerformanceCell, OrbitError> {
    let text = |key: &str| {
        cell[key].as_str().map(str::to_string).ok_or_else(|| {
            OrbitError::Execution(format!("crew performance cell missing `{key}`: {cell}"))
        })
    };
    let number = |key: &str| {
        cell[key].as_f64().ok_or_else(|| {
            OrbitError::Execution(format!("crew performance cell missing `{key}`: {cell}"))
        })
    };
    Ok(orbit_store::contracts::CrewPerformanceCell {
        crew: text("crew")?,
        model: text("model")?,
        tier: text("tier")?,
        task_type: text("type")?,
        n: cell["n"].as_u64().ok_or_else(|| {
            OrbitError::Execution(format!("crew performance cell missing `n`: {cell}"))
        })?,
        raw: number("raw")?,
        crew_mean: number("crew_mean")?,
        score: number("score")?,
    })
}

/// One row of a role's ranking and, for each term, its count, contribution
/// and the tasks behind it. `model` may be omitted when the crew has one row.
/// A missing crew, or a row the window does not contain, is invalid input.
/// `cache_ttl` is reported as on [`agent_scorecard`].
pub fn agent_detail(
    facts: &AgentFacts,
    role: AgentRole,
    query: &AgentQuery,
    crew: &str,
    model: Option<&str>,
    cache_ttl: Duration,
) -> Result<Value, OrbitError> {
    let crew = crew.trim();
    if crew.is_empty() {
        return Err(OrbitError::InvalidInput("crew is required".to_string()));
    }
    let model = model.map(str::trim).filter(|model| !model.is_empty());
    let since = query.since();
    let spec = roles::spec(role);
    let (fleet_average, scored) = match spec.rows {
        Some(rows) => score::score(spec.terms, rows(facts, since), query.min_support),
        None => (None, Vec::new()),
    };
    let matching: Vec<_> = scored
        .iter()
        .filter(|row| row.input.crew == crew && model.is_none_or(|model| row.is(crew, model)))
        .collect();
    let row = match matching.as_slice() {
        [row] => row,
        [] => {
            return Err(OrbitError::InvalidInput(format!(
                "no {} row for crew '{crew}'{} in the {} window",
                role.as_str(),
                model
                    .map(|model| format!(" and model '{model}'"))
                    .unwrap_or_default(),
                query.window,
            )));
        }
        _ => {
            // Core cannot tell which surface called, so the refusal names each
            // surface's selector: `--model` on the CLI, `row_model` over MCP and
            // `orbit tool run`, and the dashboard's detail `model` query parameter.
            let candidates: Vec<&str> = matching
                .iter()
                .map(|row| row.input.model.as_str())
                .collect();
            return Err(OrbitError::InvalidInput(format!(
                "crew '{crew}' has {} {} rows in the {} window; name one with --model (CLI), row_model (MCP and orbit tool run) or the model query parameter (dashboard API). Candidate models: {}",
                matching.len(),
                role.as_str(),
                query.window,
                candidates.join(", "),
            )));
        }
    };
    let mut payload = role_header(facts, role, query, since, cache_ttl);
    payload["fleet_average"] = json!(fleet_average.map(score::number));
    payload["provisional"] = json!(score::provisional(role, &scored));
    payload["row"] = row.row(spec.terms);
    payload["terms"] = json!(row.terms(spec.terms));
    Ok(payload)
}

/// The window's problems by owning role, plus Not scored by reason, and
/// each role's problems by row. Its role totals are the problems behind the
/// four scorecards.
pub fn agent_attribution(facts: &AgentFacts, query: &AgentQuery, cache_ttl: Duration) -> Value {
    let since = query.since();
    let mut payload = attribution::strip(facts, since);
    payload["window"] = json!(query.window);
    payload["requested_window"] = json!(query.requested);
    payload["computed_at"] = json!(
        facts
            .computed_at
            .to_rfc3339_opts(SecondsFormat::Millis, true)
    );
    payload["cache_ttl_seconds"] = json!(cache_ttl.as_secs());
    payload
}
