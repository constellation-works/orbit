//! Codebase analytics: which parts of the repository are expensive or risky
//! for agents to change, from records Orbit already keeps. The dashboard's
//! Analytics › Codebase view, `orbit codebase` and the `orbit.codebase.*`
//! tools all read this one service.
//!
//! "Touched" means a task's declared footprint (`context_files`, widened
//! selectors included), not the diff that landed. The related-file query
//! separately measures verified landed diffs, including unreserved Markdown,
//! and the task-pilot ranking grades each pilot's selectors against them.
//! [`OrbitRuntime::codebase_scan`] reads every task bundle's history once;
//! [`codebase_analytics`] and [`codebase_path_detail`] only slice those facts
//! by window, minimum support and rollup, so a caller that keeps the facts
//! (the dashboard) answers another selection without rescanning.

mod detail;
mod facts;
mod landed;
mod lock_waits;
mod pilot;
mod related;
mod sections;

use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::OrbitRuntime;

pub use facts::CodebaseFacts;
use sections::{Params, ROW_LIMIT, Rollup};

const DEFAULT_WINDOW: &str = "90d";
const DEFAULT_MIN_SUPPORT: usize = 15;
const WINDOWS: &[(&str, Option<i64>)] = &[
    ("7d", Some(7)),
    ("30d", Some(30)),
    ("90d", Some(90)),
    ("all", None),
];

/// A validated selection: the window, the smallest denominator a ranked row
/// needs, and how paths are grouped.
#[derive(Debug, Clone, Copy)]
pub struct CodebaseQuery {
    window: &'static str,
    days: Option<i64>,
    min_support: usize,
    rollup: Rollup,
}

impl CodebaseQuery {
    /// Validate a selection. An omitted window is `90d`, an omitted minimum
    /// support 15 and an omitted rollup `file`.
    pub fn parse(
        window: Option<&str>,
        min_support: Option<usize>,
        rollup: Option<&str>,
    ) -> Result<Self, OrbitError> {
        let window = window.unwrap_or(DEFAULT_WINDOW);
        let Some(&(window, days)) = WINDOWS.iter().find(|(name, _)| *name == window) else {
            return Err(OrbitError::InvalidInput(format!(
                "window must be one of 7d, 30d, 90d or all; got '{window}'"
            )));
        };
        let min_support = min_support.unwrap_or(DEFAULT_MIN_SUPPORT);
        if min_support == 0 {
            return Err(OrbitError::InvalidInput(
                "min_support must be at least 1".to_string(),
            ));
        }
        let rollup_name = rollup.unwrap_or("file");
        let Some(rollup) = Rollup::parse(rollup_name) else {
            return Err(OrbitError::InvalidInput(format!(
                "rollup must be one of file, dir or crate; got '{rollup_name}'"
            )));
        };
        Ok(Self {
            window,
            days,
            min_support,
            rollup,
        })
    }

    /// The slicing parameters; the window ends now.
    fn params(&self) -> Params {
        Params {
            since: self
                .days
                .map(|days| Utc::now() - chrono::Duration::days(days)),
            min_support: self.min_support,
            rollup: self.rollup,
        }
    }

    /// The selection and scan facts every response carries.
    fn header(&self, facts: &CodebaseFacts, since: Option<DateTime<Utc>>) -> Value {
        json!({
            "window": self.window,
            "since": since.map(|since| since.to_rfc3339_opts(SecondsFormat::Secs, true)),
            "min_support": self.min_support,
            "rollup": self.rollup.as_str(),
            "touched": "declared_footprint",
            "computed_at": facts.computed_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            "tasks_scanned": facts.tasks.len(),
            "row_limit": ROW_LIMIT,
        })
    }
}

impl OrbitRuntime {
    /// Scan the workspace's tasks, reservations and lock denials once. A task
    /// whose history cannot be read keeps its envelope facts.
    pub fn codebase_scan(&self) -> Result<CodebaseFacts, OrbitError> {
        facts::scan(self)
    }

    /// Scan, then rank for `query`. For callers that keep no scan, so the
    /// response reports a cache period of zero.
    pub fn codebase_analytics(&self, query: &CodebaseQuery) -> Result<Value, OrbitError> {
        Ok(codebase_analytics(
            &self.codebase_scan()?,
            query,
            Duration::ZERO,
        ))
    }

    /// Scan, then report one path's standing in every ranking.
    pub fn codebase_path_detail(
        &self,
        query: &CodebaseQuery,
        path: &str,
    ) -> Result<Value, OrbitError> {
        codebase_path_detail(&self.codebase_scan()?, query, path)
    }
}

/// The six path rankings, the task-pilot ranking and the paths several
/// path rankings share (`overlap`), with the
/// selection and scan facts. `cache_ttl` is how long the caller keeps
/// `facts`, reported as `cache_ttl_seconds`.
pub fn codebase_analytics(
    facts: &CodebaseFacts,
    query: &CodebaseQuery,
    cache_ttl: Duration,
) -> Value {
    let params = query.params();
    let mut payload = sections::all_sections(facts, &params);
    payload = sections::merge(payload, query.header(facts, params.since));
    payload["known_paths"] = json!(related::known_paths(facts));
    payload["cache_ttl_seconds"] = json!(cache_ttl.as_secs());
    payload
}

/// One path's rate, support and place in every ranking, with the tasks (or
/// requesters) behind each numerator. A directory or crate key is accepted
/// with or without its trailing `/`; a path nothing in the window touched is
/// invalid input.
pub fn codebase_path_detail(
    facts: &CodebaseFacts,
    query: &CodebaseQuery,
    path: &str,
) -> Result<Value, OrbitError> {
    let params = query.params();
    let detail = detail::path_detail(facts, &params, path)?;
    Ok(sections::merge(detail, query.header(facts, params.since)))
}

/// Rank files that changed alongside `path` in landed tasks. Related queries
/// always use file paths, independently of the analytics rollup.
pub fn codebase_related(
    facts: &CodebaseFacts,
    query: &CodebaseQuery,
    path: &str,
    top: usize,
) -> Result<Value, OrbitError> {
    let params = query.params();
    let payload = related::related(facts, &params, path, top)?;
    Ok(sections::merge(query.header(facts, params.since), payload))
}

impl OrbitRuntime {
    /// Scan and rank files that changed alongside one repository file.
    pub fn codebase_related(
        &self,
        query: &CodebaseQuery,
        path: &str,
        top: usize,
    ) -> Result<Value, OrbitError> {
        codebase_related(&self.codebase_scan()?, query, path, top)
    }
}
