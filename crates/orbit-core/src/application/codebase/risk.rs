//! Regression-risk marks: the context files of a task that regressed often
//! enough lately that its implementer and reviewer are told, and how marked
//! files fare once agents are.
//!
//! A mark is pushed into the prompt, compact and gated; the drill-down stays
//! with `orbit codebase show` and the `orbit.codebase.*` tools. A `file:`
//! selector's path is marked when, among the tasks done in the trailing
//! window whose declared footprint named it, at least `min_support` did so
//! and at least `threshold_percent` of them were named by a
//! `regression_from` link. Low-support paths and hub files every task
//! touches therefore stay unmarked, and a fixed hotspot drops out once its
//! regressed tasks age out of the window. At most `max_files` paths are
//! marked, by the Regressions ranking's order.
//!
//! Each mark carries the two natures and escapes most of its links record
//! (an unrecorded one reads `unclassified`, never a guess) and one lesson:
//! the newest regression task's title, with internal task and friction IDs
//! removed. Every prompt that carries marks records them on the task as a
//! `regression_risk_marked` history event, which the Regressions ranking
//! reads back to compare marked paths' rates with marks against the window
//! before the first mark.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_common::text::floor_char_boundary;
use orbit_config::RegressionRiskSettings;
use orbit_types::task::{
    REGRESSION_RISK_MARKED_EVENT, RegressionRiskMarks, TaskDecision, TaskHistoryEntry,
};
use regex::Regex;
use serde_json::{Value, json};

use super::facts::{Anchor, CodebaseFacts, TaskFacts};
use super::regression::{RegressionFilter, RegressionLink, top_categories};
use super::sections::{Params, ROW_LIMIT, Rollup, Tally, rank, rate_fields, regression_rows};
use crate::OrbitRuntime;
use crate::application::task::SYSTEM_ACTOR_LABEL;

/// Natures and escapes one mark lists.
const CATEGORY_LIMIT: usize = 2;
/// Longest lesson line, in bytes, before it is cut with an ellipsis.
const LESSON_MAX_BYTES: usize = 160;

/// One marked path, before rendering.
struct Mark<'a> {
    path: String,
    tally: Tally,
    /// Every link naming one of the path's regressed tasks.
    links: Vec<&'a RegressionLink>,
}

impl OrbitRuntime {
    /// The `regression_risk` section of the prompt `run_id` builds for
    /// `activity` on `task_id`, whose canonical context selectors are
    /// `selectors`, or `None` when marks are off or no `file:` path
    /// qualifies. Marks it returns are recorded on the task first; a record
    /// that cannot be written is logged and does not withhold them.
    pub(crate) fn regression_risk_section(
        &self,
        task_id: &str,
        selectors: &[String],
        run_id: &str,
        activity: &str,
    ) -> Result<Option<Value>, OrbitError> {
        let settings = self.regression_risk_settings();
        let paths: BTreeSet<String> = selectors
            .iter()
            .filter(|selector| selector.trim_start().starts_with("file:"))
            .filter_map(|selector| Anchor::parse(selector))
            .map(|anchor| anchor.path)
            .collect();
        if !settings.enabled || paths.is_empty() {
            return Ok(None);
        }
        let since = Utc::now() - Duration::days(i64::from(settings.window_days));
        let facts = super::facts::scan_regressions(self, since)?;
        let marks = marks(&facts, &paths, &settings, since);
        if marks.is_empty() {
            return Ok(None);
        }
        self.record_marks(
            task_id,
            RegressionRiskMarks {
                run_id: run_id.to_string(),
                activity: activity.to_string(),
                paths: marks.iter().map(|mark| mark.path.clone()).collect(),
                window_days: settings.window_days,
            },
        );
        let scrubber = IdScrubber::new(&facts);
        let rendered: Vec<Value> = marks.iter().map(|mark| render(mark, &scrubber)).collect();
        Ok(Some(json!({
            "window_days": settings.window_days,
            "marks": rendered,
        })))
    }

    /// Record what one prompt marked, once per run, activity and paths. A
    /// claimed leaf records nothing: it cannot write its owner's task, and
    /// the owner's analytics read only the owner's records.
    fn record_marks(&self, task_id: &str, marks: RegressionRiskMarks) {
        if self.worker_invocation().is_some() {
            return;
        }
        let recorded = self.get_task_history(task_id).is_ok_and(|history| {
            history.iter().any(|entry| {
                matches!(&entry.decision, Some(TaskDecision::RegressionRiskMarked(earlier)) if *earlier == marks)
            })
        });
        if recorded {
            return;
        }
        let note = json!({
            "run_id": marks.run_id,
            "activity": marks.activity,
            "paths": marks.paths,
            "window_days": marks.window_days,
        });
        let entry = TaskHistoryEntry {
            at: Utc::now(),
            by: SYSTEM_ACTOR_LABEL.to_string(),
            event: REGRESSION_RISK_MARKED_EVENT.to_string(),
            note: Some(note.to_string()),
            from_status: None,
            to_status: None,
            operation_id: None,
            decision: Some(TaskDecision::RegressionRiskMarked(marks)),
        };
        if let Err(error) = self.append_system_history(task_id, vec![entry]) {
            tracing::warn!(%task_id, %error, "regression risk: marks not recorded");
        }
    }
}

/// The paths of `paths` that qualify, highest risk first, at most
/// `max_files` of them.
fn marks<'a>(
    facts: &'a CodebaseFacts,
    paths: &BTreeSet<String>,
    settings: &RegressionRiskSettings,
    since: DateTime<Utc>,
) -> Vec<Mark<'a>> {
    let min_support = settings.min_support as usize;
    let params = Params {
        since: Some(since),
        min_support,
        rollup: Rollup::File,
        filter: RegressionFilter::default(),
    };
    let (mut rows, _) = regression_rows(facts, &params);
    rows.retain(|path, row| {
        let Tally {
            numerator,
            denominator,
        } = row.tally;
        paths.contains(path)
            && numerator > 0
            && numerator * 100 >= usize::from(settings.threshold_percent) * denominator
    });
    let (ranked, _) = rank(rows, min_support, |row| {
        (row.tally.numerator, row.tally.denominator)
    });
    ranked
        .into_iter()
        .take(settings.max_files as usize)
        .map(|(path, row)| Mark {
            path,
            links: row
                .regressed
                .iter()
                .flat_map(|task| task.regressions.iter())
                .collect(),
            tally: row.tally,
        })
        .collect()
}

fn render(mark: &Mark, scrubber: &IdScrubber) -> Value {
    let (natures, escapes) = top_categories(&mark.links, CATEGORY_LIMIT);
    let mut rendered = json!({
        "path": mark.path,
        "regressed": format!(
            "{} of {} touching tasks",
            mark.tally.numerator, mark.tally.denominator
        ),
        "nature": natures,
        "escape": escapes,
    });
    let lesson = mark
        .links
        .iter()
        .max_by(|left, right| {
            left.filed_at
                .cmp(&right.filed_at)
                .then_with(|| left.task_id.cmp(&right.task_id))
        })
        .and_then(|newest| scrubber.lesson(&newest.summary));
    if let Some(lesson) = lesson {
        rendered["lesson"] = json!(lesson);
    }
    rendered
}

/// Removes internal task and friction IDs from text a prompt shows: IDs
/// under every task prefix the scanned workspace uses, legacy `T<date>-<n>`
/// task IDs and `F<year>-<month>-<n>` friction IDs, with their brackets.
struct IdScrubber {
    ids: Regex,
    leftovers: Regex,
}

impl IdScrubber {
    fn new(facts: &CodebaseFacts) -> Self {
        let prefixes: BTreeSet<String> = facts
            .tasks
            .iter()
            .filter_map(|task| task.id.rsplit_once('-'))
            .filter(|(prefix, number)| {
                !prefix.is_empty()
                    && prefix.chars().all(|c| c.is_ascii_alphabetic())
                    && !number.is_empty()
                    && number.chars().all(|c| c.is_ascii_digit())
            })
            .map(|(prefix, _)| regex::escape(prefix))
            .collect();
        let mut alternatives = vec![r"T\d{8}-\d+".to_string(), r"F\d{4}-\d{2}-\d+".to_string()];
        alternatives.extend(prefixes.into_iter().map(|prefix| format!(r"{prefix}-\d+")));
        let ids = format!(r"\[?\b(?:{})\b\]?", alternatives.join("|"));
        Self {
            // Built from escaped prefixes and fixed alternatives.
            ids: Regex::new(&ids).unwrap_or_else(|_| never_matches()),
            leftovers: Regex::new(r"\(\s*\)|\[\s*\]").unwrap_or_else(|_| never_matches()),
        }
    }

    /// One lesson line from a regression summary, or `None` when nothing is
    /// left once IDs are removed.
    fn lesson(&self, summary: &str) -> Option<String> {
        let first_line = summary.lines().next().unwrap_or_default();
        let without_ids = self.ids.replace_all(first_line, "");
        let without_leftovers = self.leftovers.replace_all(&without_ids, "");
        let collapsed = without_leftovers
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let trimmed = collapsed.trim_matches(|c: char| {
            c.is_whitespace() || matches!(c, ':' | ',' | ';' | '-' | '–' | '—')
        });
        if trimmed.is_empty() {
            return None;
        }
        if trimmed.len() <= LESSON_MAX_BYTES {
            return Some(trimmed.to_string());
        }
        let cut = floor_char_boundary(trimmed, LESSON_MAX_BYTES);
        Some(format!("{}…", trimmed[..cut].trim_end()))
    }
}

/// A pattern that matches nothing, for a pattern that failed to build.
fn never_matches() -> Regex {
    Regex::new(r"[^\s\S]").unwrap_or_else(|_| unreachable!("a fixed empty class always builds"))
}

/// Marked paths' regression rates with marks against the window before the
/// first mark, merged into the `regressions` section as `risk_marks`.
///
/// "With marks" is the done tasks of the selected window a prompt marked
/// the path for; "before" is the done tasks that touched the path in the
/// marks' trailing window before the first mark was recorded. Both count a
/// task as regressed when a link the filter keeps names it, and both show
/// their counts so a small sample reads as one.
pub(super) fn comparison(facts: &CodebaseFacts, params: &Params) -> Value {
    let first = facts
        .tasks
        .iter()
        .flat_map(|task| task.marks.iter())
        .min_by_key(|(at, _)| *at);
    let Some((enabled_at, first)) = first else {
        return json!({
            "enabled_at": null,
            "before_since": null,
            "window_days": null,
            "marked_paths": 0,
            "with_marks": rate_fields(0, 0),
            "before": rate_fields(0, 0),
            "rows": [],
        });
    };
    let enabled_at = *enabled_at;
    let before_since = enabled_at - Duration::days(i64::from(first.window_days));

    let mut with_marks: BTreeMap<&str, Vec<&TaskFacts>> = BTreeMap::new();
    for task in &facts.tasks {
        if !task.done_at.is_some_and(|at| params.within(at)) {
            continue;
        }
        let marked: BTreeSet<&str> = task
            .marks
            .iter()
            .flat_map(|(_, marks)| marks.paths.iter().map(String::as_str))
            .collect();
        for path in marked {
            with_marks.entry(path).or_default().push(task);
        }
    }
    let mut before: BTreeMap<&str, Vec<&TaskFacts>> = BTreeMap::new();
    for task in &facts.tasks {
        if !task
            .done_at
            .is_some_and(|at| at >= before_since && at < enabled_at)
        {
            continue;
        }
        for key in Rollup::File.keys(&task.footprint) {
            if let Some((path, _)) = with_marks.get_key_value(key.as_str()) {
                before.entry(path).or_default().push(task);
            }
        }
    }

    let filter = &params.filter;
    let tally = |tasks: &[&TaskFacts]| {
        let regressed = tasks.iter().filter(|task| filter.regressed(task)).count();
        rate_fields(regressed, tasks.len())
    };
    let distinct = |by_path: &BTreeMap<&str, Vec<&TaskFacts>>| {
        let mut seen = BTreeSet::new();
        let tasks: Vec<&TaskFacts> = by_path
            .values()
            .flatten()
            .copied()
            .filter(|task| seen.insert(task.id.as_str()))
            .collect();
        tally(&tasks)
    };
    let mut rows: Vec<(&str, &Vec<&TaskFacts>)> = with_marks
        .iter()
        .map(|(path, tasks)| (*path, tasks))
        .collect();
    rows.sort_by(|(left_path, left), (right_path, right)| {
        right
            .len()
            .cmp(&left.len())
            .then_with(|| left_path.cmp(right_path))
    });
    let rows: Vec<Value> = rows
        .into_iter()
        .take(ROW_LIMIT)
        .map(|(path, tasks)| {
            json!({
                "path": path,
                "with_marks": tally(tasks),
                "before": tally(before.get(path).map(Vec::as_slice).unwrap_or_default()),
            })
        })
        .collect();
    json!({
        "enabled_at": enabled_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        "before_since": before_since.to_rfc3339_opts(SecondsFormat::Secs, true),
        "window_days": first.window_days,
        "marked_paths": with_marks.len(),
        "with_marks": distinct(&with_marks),
        "before": distinct(&before),
        "rows": rows,
    })
}
