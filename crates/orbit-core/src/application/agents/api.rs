use std::time::Duration;

use chrono::SecondsFormat;
use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::{AgentFacts, AgentQuery, AgentRole, WindowBounds, attribution, facts, roles, score};
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
            self.task_types(),
            Duration::ZERO,
        ))
    }

    /// Scan, then export one row per implemented task, with a cache period
    /// of zero as on [`Self::agent_role_scorecard`]. Read-only: it writes no
    /// snapshot and refreshes no cache beyond that scan.
    pub fn agent_per_task_export(&self, query: &AgentQuery) -> Result<Value, OrbitError> {
        Ok(agent_per_task_export(
            &self.agent_scan()?,
            query,
            self.task_types(),
            Duration::ZERO,
        ))
    }

    pub(crate) fn agent_role_detail_with_effort(
        &self,
        role: AgentRole,
        query: &AgentQuery,
        crew: &str,
        model: Option<&str>,
        effort: Option<&str>,
        surface: AgentDetailSurface,
    ) -> Result<Value, OrbitError> {
        agent_detail_for_surface(
            &self.agent_scan()?,
            role,
            query,
            crew,
            model,
            effort,
            Duration::ZERO,
            surface,
        )
    }
}

/// Recovery controls available to the caller of an agent detail request.
#[derive(Clone, Copy)]
pub(crate) enum AgentDetailSurface {
    Cli,
    ToolRun,
    Mcp,
    Dashboard,
}

impl AgentDetailSurface {
    fn missing_row_recovery(self, role: AgentRole) -> String {
        let role = role.as_str();
        match self {
            Self::Cli => {
                format!("list rows with orbit agents scorecard --role {role} or widen --window")
            }
            Self::ToolRun => format!(
                "list rows with orbit.agents.scorecard (role {role}) or widen the window field"
            ),
            Self::Mcp => format!(
                "list rows with orbit_agents_scorecard (role {role}) or widen the window field"
            ),
            Self::Dashboard => format!(
                "list rows at /api/agents/{role}/scorecard or widen the window query parameter"
            ),
        }
    }
}

/// A role's scorecard header: selection, unit, terms and status.
pub(super) fn role_header(
    facts: &AgentFacts,
    role: AgentRole,
    query: &AgentQuery,
    window: WindowBounds,
    cache_ttl: Duration,
) -> Value {
    let spec = roles::spec(role);
    let mut payload = query.header(facts, window, cache_ttl);
    payload["role"] = json!(role.as_str());
    payload["effort_breakdown"] = json!(orbit_store::contracts::agent_effort_usage(
        facts
            .invocations
            .iter_invocations()
            .filter(|fact| window.admits(fact.ts))
    ));
    payload["effort_population"] = json!("completed_workspace_invocations");
    payload["unit"] = json!(spec.unit);
    payload["status"] = json!(if spec.rows.is_some() {
        "ok"
    } else {
        "not_computed"
    });
    payload["terms"] = json!(score::terms_json(spec.terms, spec.term_units));
    if let Some(header) = spec.header {
        for (key, value) in header(facts, window) {
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
    let window = query.bounds(facts);
    let spec = roles::spec(role);
    let mut payload = role_header(facts, role, query, window, cache_ttl);
    let (fleet_average, scored) = match spec.rows {
        Some(rows) => score::score(
            spec.terms,
            rows(facts, window, query.min_support, query.execution),
            query.min_support,
        ),
        None => (None, Vec::new()),
    };
    payload["fleet_average"] = json!(fleet_average.map(score::number));
    payload["provisional"] = json!(score::provisional(role, &scored));
    payload["rows"] = scored
        .iter()
        .map(|row| {
            let mut value = row.row(spec.terms);
            value["effort_breakdown"] = row_effort_breakdown(
                facts,
                role,
                window,
                &row.input.crews,
                &row.input.model,
                &row.input.effort,
            );
            value
        })
        .collect();
    payload
}

/// The implementer cell matrix for `query`: one cell per `(crew, model,
/// tier, type)` with its `n`, `raw`, `crew_mean` and shrunk `score`, plus
/// the type list the tasks were classified with.
pub fn agent_by_type_scorecard(
    facts: &AgentFacts,
    query: &AgentQuery,
    types: &[orbit_config::TaskTypeDefinition],
    cache_ttl: Duration,
) -> Value {
    let window = query.bounds(facts);
    let role = AgentRole::Implementers;
    let mut payload = role_header(facts, role, query, window, cache_ttl);
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
    payload["cells"] = json!(roles::implementer_by_type_cells(
        facts,
        window,
        types,
        query.execution
    ));
    payload
}

/// The implementer regression rate by landed change size for `query`: one
/// series per crew reference as dispatched, each with its
/// `floor(log2(1 + lines))` buckets, task and regressed counts, rate and
/// Wilson 95% interval, flagged `drawn` once a bucket has enough tasks and
/// `sparse` under five outcomes. `complexity` narrows the cohort to one
/// tier; an unknown tier is invalid input.
pub fn agent_size_curves(
    facts: &AgentFacts,
    query: &AgentQuery,
    complexity: Option<&str>,
    cache_ttl: Duration,
) -> Result<Value, OrbitError> {
    let complexity = complexity.map(str::trim).filter(|tier| !tier.is_empty());
    if let Some(tier) = complexity
        && !roles::SIZE_CURVE_TIERS.contains(&tier)
    {
        return Err(OrbitError::InvalidInput(format!(
            "complexity must be one of {}; got '{tier}'",
            roles::SIZE_CURVE_TIERS.join(", ")
        )));
    }
    let window = query.bounds(facts);
    let mut payload = query.header(facts, window, cache_ttl);
    payload["role"] = json!(AgentRole::Implementers.as_str());
    for (key, value) in roles::implementer_size_curves(facts, window, complexity, query.execution) {
        payload[key] = value;
    }
    Ok(payload)
}

/// The per-task implementer export for `query`: one row per implemented
/// task and the crew and model that worked it, with the type list the
/// tasks were classified with. The offline counterpart of the cell
/// matrix; summed per `(crew, model, tier, type)` it reproduces it.
pub fn agent_per_task_export(
    facts: &AgentFacts,
    query: &AgentQuery,
    types: &[orbit_config::TaskTypeDefinition],
    cache_ttl: Duration,
) -> Value {
    let window = query.bounds(facts);
    let role = AgentRole::Implementers;
    let mut payload = role_header(facts, role, query, window, cache_ttl);
    payload["per_task"] = json!(true);
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
    payload["tasks"] = json!(roles::implementer_per_task_rows(
        facts,
        window,
        types,
        query.execution
    ));
    payload
}

/// One row of a role's ranking and, for each term, its count, contribution
/// and the tasks behind it. `model` may be omitted when the crew has one row.
/// A missing crew, or a row the window does not contain, is invalid input.
/// `cache_ttl` is reported as on [`agent_scorecard`]. Missing-row recovery
/// names the dashboard API controls; tool adapters select their own surface.
pub fn agent_detail(
    facts: &AgentFacts,
    role: AgentRole,
    query: &AgentQuery,
    crew: &str,
    model: Option<&str>,
    cache_ttl: Duration,
) -> Result<Value, OrbitError> {
    agent_detail_for_surface(
        facts,
        role,
        query,
        crew,
        model,
        None,
        cache_ttl,
        AgentDetailSurface::Dashboard,
    )
}

/// [`agent_detail`] with an effort selection: a wire effort name,
/// `provider_default` or `unknown`, narrowing to one model-and-effort row.
pub fn agent_detail_with_effort(
    facts: &AgentFacts,
    role: AgentRole,
    query: &AgentQuery,
    crew: &str,
    model: Option<&str>,
    effort: Option<&str>,
    cache_ttl: Duration,
) -> Result<Value, OrbitError> {
    agent_detail_for_surface(
        facts,
        role,
        query,
        crew,
        model,
        effort,
        cache_ttl,
        AgentDetailSurface::Dashboard,
    )
}

#[allow(clippy::too_many_arguments)]
fn agent_detail_for_surface(
    facts: &AgentFacts,
    role: AgentRole,
    query: &AgentQuery,
    crew: &str,
    model: Option<&str>,
    effort: Option<&str>,
    cache_ttl: Duration,
    surface: AgentDetailSurface,
) -> Result<Value, OrbitError> {
    let crew = crew.trim();
    if crew.is_empty() {
        return Err(OrbitError::InvalidInput("crew is required".to_string()));
    }
    let model = model.map(str::trim).filter(|model| !model.is_empty());
    let window = query.bounds(facts);
    let spec = roles::spec(role);
    let (fleet_average, scored) = match spec.rows {
        Some(rows) => score::score(
            spec.terms,
            rows(facts, window, query.min_support, query.execution),
            query.min_support,
        ),
        None => (None, Vec::new()),
    };
    let effort = effort
        .map(str::trim)
        .filter(|effort| !effort.is_empty())
        .map(super::effort::parse_recorded_effort);
    let matching: Vec<_> = scored
        .iter()
        .filter(|row| {
            row.has_crew(crew)
                && model.is_none_or(|model| row.input.model == model)
                && effort
                    .as_ref()
                    .is_none_or(|effort| &row.input.effort == effort)
        })
        .collect();
    let row = match matching.as_slice() {
        [row] => row,
        [] => {
            return Err(OrbitError::InvalidInput(format!(
                "no {} row for crew '{crew}'{} in the {} window; {}. A crew with no work in the window has no row",
                role.as_str(),
                model
                    .map(|model| format!(" and model '{model}'"))
                    .unwrap_or_default(),
                query.window,
                surface.missing_row_recovery(role),
            )));
        }
        _ => {
            // Keep the existing model-selection guidance for ambiguous rows:
            // `--model` on the CLI, `row_model` over MCP and `orbit tool run`,
            // and the dashboard's detail `model` query parameter.
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
    let mut payload = role_header(facts, role, query, window, cache_ttl);
    payload["fleet_average"] = json!(fleet_average.map(score::number));
    payload["provisional"] = json!(score::provisional(role, &scored));
    payload["row"] = row.row(spec.terms);
    payload["row"]["effort_breakdown"] = row_effort_breakdown(
        facts,
        role,
        window,
        &row.input.crews,
        &row.input.model,
        &row.input.effort,
    );
    payload["terms"] = json!(row.terms(spec.terms));
    Ok(payload)
}

/// The window's problems by owning role, plus Not scored by reason, and
/// each role's problems by row. Its role totals are the problems behind the
/// three scorecards.
pub fn agent_attribution(facts: &AgentFacts, query: &AgentQuery, cache_ttl: Duration) -> Value {
    let window = query.bounds(facts);
    let mut payload = attribution::strip(facts, window);
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

fn row_effort_breakdown(
    facts: &AgentFacts,
    role: AgentRole,
    window: WindowBounds,
    crews: &[String],
    model: &str,
    _effort: &super::effort::RecordedEffort,
) -> Value {
    let crew = crews.first().map(String::as_str).unwrap_or_default();
    let rows = orbit_store::contracts::agent_effort_usage(
        facts.invocations.iter_invocations().filter(|fact| {
            window.admits(fact.ts)
                && fact
                    .execution
                    .as_ref()
                    .and_then(|execution| execution.crew.as_deref())
                    .is_some_and(|crew| crews.iter().any(|name| name == crew))
                && fact.model.as_deref() == Some(model)
                && match role {
                    AgentRole::Implementers => matches!(
                        fact.activity_id.as_deref(),
                        Some("implement_one" | "agent_implement")
                    ),
                    AgentRole::Reviewers => fact
                        .activity_id
                        .as_deref()
                        .is_some_and(|id| id.contains("review")),
                    AgentRole::Pilots => fact
                        .activity_id
                        .as_deref()
                        .is_some_and(|id| id.contains("pilot")),
                }
        }),
    );
    if rows.is_empty() {
        json!([{"crew": crew, "model": model, "reasoning_effort": "unknown", "source": "no_dispatch_evidence"}])
    } else {
        json!(rows)
    }
}
