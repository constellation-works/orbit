//! [model ranking §5.1] The read surface over required-check failures the
//! landing recorded by cause: `orbit agents ci-failures`, the
//! `orbit.agents.ci_failures` tool and `GET /api/agents/ci-failures`.
//!
//! Each task's history holds its `ci_failure_recorded` and
//! `ci_failure_reclassified` events; [`ci_failure_ledger`] folds them into
//! one entry per head and check with its current class and audited changes.
//! The selection only filters those entries: by when the failure was first
//! recorded, by task and by current class.

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::{CiFailureClass, CiFailureLedgerEntry, ci_failure_ledger};
use serde_json::{Value, json};

use crate::OrbitRuntime;

const DEFAULT_WINDOW: &str = "30d";

/// A validated selection of recorded CI failures.
#[derive(Debug, Clone)]
pub struct CiFailureQuery {
    window: String,
    /// Inclusive start; `None` for `all`.
    since: Option<DateTime<Utc>>,
    /// Exclusive end; `None` for now.
    until: Option<DateTime<Utc>>,
    task: Option<String>,
    class: Option<CiFailureClass>,
}

impl CiFailureQuery {
    /// Validate a selection. `window` is `all` or a duration such as `14d`
    /// (units `m`, `h`, `d`, `w`; default `30d`) ending at `until`, which is
    /// an RFC 3339 instant or a duration ago and defaults to now. `class` is
    /// `code`, `flaky`, `infrastructure` or `unknown`.
    pub fn parse(
        window: Option<&str>,
        until: Option<&str>,
        task: Option<&str>,
        class: Option<&str>,
    ) -> Result<Self, OrbitError> {
        let now = Utc::now();
        let until = match until.map(str::trim) {
            None => None,
            Some(raw) => Some(super::query::parse_until(raw, now)?),
        };
        let window = window
            .map(str::trim)
            .filter(|window| !window.is_empty())
            .unwrap_or(DEFAULT_WINDOW)
            .to_string();
        let since = if window == "all" {
            None
        } else {
            let refused = || {
                OrbitError::InvalidInput(format!(
                    "window must be all or a duration such as 30d (units m, h, d or w); got '{window}'"
                ))
            };
            if !window.starts_with(|c: char| c.is_ascii_digit()) {
                return Err(refused());
            }
            Some(super::query::parse_until(&window, until.unwrap_or(now)).map_err(|_| refused())?)
        };
        let class = match class.map(str::trim).filter(|class| !class.is_empty()) {
            None => None,
            Some(raw) => Some(CiFailureClass::parse(raw).ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "class must be one of code, flaky, infrastructure or unknown; got '{raw}'"
                ))
            })?),
        };
        let task = task
            .map(str::trim)
            .filter(|task| !task.is_empty())
            .map(str::to_string);
        Ok(Self {
            window,
            since,
            until,
            task,
            class,
        })
    }

    fn in_window(&self, entry: &CiFailureLedgerEntry) -> bool {
        self.since.is_none_or(|since| entry.recorded_at >= since)
            && self.until.is_none_or(|until| entry.recorded_at < until)
    }
}

impl OrbitRuntime {
    /// The recorded required-check failures `query` selects, newest first,
    /// with per-class counts over the window and task (before the class
    /// filter). A task whose history cannot be read is skipped
    /// and named in `unreadable_tasks`.
    pub fn ci_failure_records(&self, query: &CiFailureQuery) -> Result<Value, OrbitError> {
        let tasks = match &query.task {
            Some(task) => {
                self.get_task(task)?;
                vec![task.clone()]
            }
            None => self
                .list_task_metadata()?
                .into_iter()
                // A recorded failure updates its task, so a task untouched
                // since the window opened holds none in it.
                .filter(|task| query.since.is_none_or(|since| task.updated_at >= since))
                .map(|task| task.id.to_string())
                .collect(),
        };
        let mut unreadable = Vec::new();
        let mut entries = Vec::new();
        for task in tasks {
            match self.get_task_history(&task) {
                Ok(history) => entries.extend(
                    ci_failure_ledger(&history)
                        .into_iter()
                        .filter(|entry| query.in_window(entry)),
                ),
                Err(error) => {
                    tracing::warn!(task_id = %task, %error, "CI failure records: task history unreadable");
                    unreadable.push(task);
                }
            }
        }
        entries.sort_by(|left, right| {
            right
                .recorded_at
                .cmp(&left.recorded_at)
                .then_with(|| left.record.task_id.cmp(&right.record.task_id))
        });
        // Counts cover the window and task, whatever class is selected, so
        // the share of each cause stays visible beside a class filter.
        let counts = CiFailureClass::ALL
            .into_iter()
            .map(|class| {
                (
                    class.as_str().to_string(),
                    json!(entries.iter().filter(|entry| entry.class == class).count()),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        entries.retain(|entry| query.class.is_none_or(|class| entry.class == class));
        let instant = |at: &DateTime<Utc>| at.to_rfc3339_opts(SecondsFormat::Secs, true);
        let records = entries
            .iter()
            .map(|entry| {
                json!({
                    "task_id": entry.record.task_id,
                    "head_sha": entry.record.head_sha,
                    "pr_number": entry.record.pr_number,
                    "producing_run_id": entry.record.producing_run_id,
                    "check": entry.record.check,
                    "signature": entry.record.signature,
                    "class": entry.class.as_str(),
                    "recorded_class": entry.record.class.as_str(),
                    "landing_class": entry.record.landing_class,
                    "evidence": entry.record.evidence,
                    "recorded_at": instant(&entry.recorded_at),
                    "changes": entry.changes.iter().map(|change| json!({
                        "at": instant(&change.at),
                        "from": change.from.as_str(),
                        "to": change.to.as_str(),
                        "evidence": change.evidence,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "window": query.window,
            "since": query.since.as_ref().map(instant),
            "until": query.until.as_ref().map(instant),
            "task": query.task,
            "class": query.class.map(CiFailureClass::as_str),
            "counts": counts,
            "records": records,
            "unreadable_tasks": unreadable,
        }))
    }
}
