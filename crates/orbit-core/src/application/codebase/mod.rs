//! Codebase analytics: which parts of the repository are expensive or risky
//! for agents to change, from records Orbit already keeps. The dashboard's
//! Analytics › Codebase view, `orbit codebase` and the `orbit.codebase.*`
//! tools all read this one service.
//!
//! "Touched" means a task's declared footprint (`context_files`, widened
//! selectors included), not the diff that landed. The related-file query
//! separately measures verified landed diffs, including unreserved Markdown.
//! The same scan supplies the evidence the Agents › Pilots scorecard grades
//! pilots by (`pilot_evidence`).
//! [`OrbitRuntime::codebase_scan`] lists every task's envelope from the task
//! index and takes what its history, comments and failed claim settlements
//! say from the store's derived per-task facts (`stored`), reading a task's
//! event log (outside the task commit lock when nothing is left to recover)
//! and diffing its landed commit only when that task changed since the last
//! scan by any process. Every scan path shares those rows; a missing or
//! outdated row is recomputed and written back. [`codebase_analytics`] and
//! [`codebase_path_detail`] only slice the facts by window, minimum support
//! and rollup. Analytics and path detail use verified landing receipts in
//! task history, without observing delivery runs. Related queries use
//! [`OrbitRuntime::codebase_related_scan`] for window-scoped delivery
//! fallback, which lists the workspace's runs and parses the job catalog once
//! per scan; what delivery observation finds is never stored with a task. A
//! caller that keeps the history-only facts (the dashboard) answers another
//! selection without rescanning.

mod detail;
mod facts;
mod landed;
mod lock_waits;
mod pilot;
mod regression;
mod related;
mod risk;
mod sections;
mod stored;

use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::OrbitRuntime;

pub use facts::CodebaseFacts;
pub(in crate::application) use facts::{
    Anchor, BlockCause, SettledFailure, actor_kind, classify_block, failed_claim_settlements_of,
};
pub(in crate::application) use pilot::{
    Contest, HeldWait, PilotEvidence, PilotRun, pilot_evidence,
};
pub use regression::RegressionFilter;
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
/// needs, how paths are grouped, and which regressions the Regressions
/// ranking counts.
#[derive(Debug, Clone, Copy)]
pub struct CodebaseQuery {
    window: &'static str,
    days: Option<i64>,
    min_support: usize,
    rollup: Rollup,
    filter: RegressionFilter,
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
            filter: RegressionFilter::default(),
        })
    }

    /// Count only the regressions `filter` keeps in the Regressions ranking,
    /// its per-path detail and its crew and complexity rates.
    pub fn with_regression_filter(mut self, filter: RegressionFilter) -> Self {
        self.filter = filter;
        self
    }

    /// The slicing parameters; the window ends now.
    fn params(&self) -> Params {
        Params {
            since: self
                .days
                .map(|days| Utc::now() - chrono::Duration::days(days)),
            min_support: self.min_support,
            rollup: self.rollup,
            filter: self.filter,
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

/// Related-file selection. The omitted window is 30 days and the omitted
/// partner cap is 10. Rollup is always `file` and regression filters are not
/// parameters. Analytics and path detail keep [`CodebaseQuery::parse`]'s
/// 90-day default and their own filters.
#[derive(Debug, Clone, Copy)]
pub struct RelatedQuery {
    query: CodebaseQuery,
    top: usize,
}

impl RelatedQuery {
    /// Landed period when `window` is omitted.
    const DEFAULT_WINDOW: &str = "30d";
    /// Partner cap when `top` is omitted.
    const DEFAULT_TOP: usize = 10;

    /// Validate a related selection.
    ///
    /// An omitted window is 30 days, an omitted minimum support is the shared
    /// analytics default (15) and an omitted `top` is 10. The selection always
    /// uses file rollup and no regression filter.
    pub fn parse(
        window: Option<&str>,
        min_support: Option<usize>,
        top: Option<usize>,
    ) -> Result<Self, OrbitError> {
        let query = CodebaseQuery::parse(
            Some(window.unwrap_or(Self::DEFAULT_WINDOW)),
            min_support,
            Some("file"),
        )?;
        Ok(Self {
            query,
            top: top.unwrap_or(Self::DEFAULT_TOP),
        })
    }

    /// Resolved window: `7d`, `30d`, `90d` or `all`.
    pub fn window(self) -> &'static str {
        self.query.window
    }

    /// Resolved partner cap.
    pub fn top(self) -> usize {
        self.top
    }

    /// File-rollup selection with no regression filter, for the related scan
    /// and ranking.
    pub fn codebase_query(self) -> CodebaseQuery {
        self.query
    }
}

impl OrbitRuntime {
    /// Scan the workspace's tasks, reservations and lock denials once, reusing
    /// the stored facts of every task unchanged since they were computed. A
    /// task whose history cannot be read keeps its envelope facts. Landed paths
    /// and pilot receipts use task-history receipts only, without delivery
    /// observation.
    pub fn codebase_scan(&self) -> Result<CodebaseFacts, OrbitError> {
        facts::scan(self, None, true)
    }

    /// Scan facts for related-file queries in this window. Delivery fallback
    /// is restricted to done tasks in the window; cache separately from the
    /// history-only analytics scan and from other windows.
    pub fn codebase_related_scan(
        &self,
        query: &CodebaseQuery,
    ) -> Result<CodebaseFacts, OrbitError> {
        facts::scan(self, Some(&query.params()), true)
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

    /// Scan history-only facts, including landed receipts, then report one
    /// path's standing in every ranking without observing delivery runs.
    pub fn codebase_path_detail(
        &self,
        query: &CodebaseQuery,
        path: &str,
    ) -> Result<Value, OrbitError> {
        codebase_path_detail(&self.codebase_scan()?, query, path)
    }
}

/// The six path rankings and the paths several of them share (`overlap`),
/// with the selection and scan facts. `cache_ttl` is how long the caller keeps
/// `facts`, reported as `cache_ttl_seconds`.
pub fn codebase_analytics(
    facts: &CodebaseFacts,
    query: &CodebaseQuery,
    cache_ttl: Duration,
) -> Value {
    let params = query.params();
    let mut payload = sections::all_sections(facts, &params);
    payload = sections::merge(payload, query.header(facts, params.since));
    payload["regression_filter"] = query.filter.json();
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
    let mut detail = detail::path_detail(facts, &params, path)?;
    detail["regression_filter"] = query.filter.json();
    Ok(sections::merge(detail, query.header(facts, params.since)))
}

/// Rank files that changed alongside `path` in landed tasks. Related queries
/// always use file paths, independently of the analytics rollup. Use facts
/// from [`OrbitRuntime::codebase_related_scan`] for the same window to include
/// delivery-run evidence; [`OrbitRuntime::codebase_scan`] contains receipts only.
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
        codebase_related(&self.codebase_related_scan(query)?, query, path, top)
    }
}
