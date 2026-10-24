use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::{AgentFacts, AgentQuery, AgentRole, attribution, facts, roles, score, shadow};
use crate::OrbitRuntime;

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
pub(super) fn role_header(
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
