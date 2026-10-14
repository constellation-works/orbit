//! CLI rendering for registry-derived task lock operations.

use crate::command::task::output::format_task_locks;
use crate::command::{Block, CommandOut, Execute, Payload};
use crate::output::table::{Column, Table};
use crate::parse::parse_duration_seconds;
use clap::Args;
use orbit_common::governance::task::TaskVerb;
use orbit_core::{OrbitError, OrbitRuntime, TaskStatus};
use serde_json::{Value, json};
use std::fmt::Write as _;

pub(crate) fn render(verb: TaskVerb, value: Value) -> CommandOut {
    match verb {
        TaskVerb::LocksList => Ok(Payload::detail(value.clone(), format_task_locks(&value)).into()),
        TaskVerb::LocksRelease => Ok(Payload::document(value).into()),
        TaskVerb::LocksReserve => {
            let exit_code = if value["reserved"].as_bool().unwrap_or(false) {
                0
            } else {
                3
            };
            Ok(Payload::blocks(value.clone(), reservation_blocks(&value))
                .with_exit_code(exit_code)
                .into())
        }
        TaskVerb::Add
        | TaskVerb::Update
        | TaskVerb::List
        | TaskVerb::Eligible
        | TaskVerb::Show
        | TaskVerb::Lint
        | TaskVerb::ArtifactPut
        | TaskVerb::ArtifactGet
        | TaskVerb::ReviewReset
        | TaskVerb::ReconcileReview
        | TaskVerb::RetryHandoff
        | TaskVerb::Reject
        | TaskVerb::Delete => Err(OrbitError::Execution(
            "task lock renderer received another operation".into(),
        )),
    }
}

/// The TTL as whole seconds. `parse_duration_seconds` accepts the same
/// `s/m/h/d/w` forms as the rest of the CLI; the domain rejects anything
/// outside 1..=7200, so only the conversion to its integer width is checked
/// here — a week-long TTL must reach the domain to be refused by name rather
/// than silently wrapping.
pub(crate) fn reservation_ttl_seconds(raw: &str) -> Result<u64, OrbitError> {
    let seconds = parse_duration_seconds(raw)?;
    if seconds == 0 {
        return Err(OrbitError::InvalidInput(
            "`--ttl` must be longer than zero".to_string(),
        ));
    }
    Ok(seconds)
}

/// Grant or denial, rendered as the operator needs to read it: what is held
/// and until when, or which selector collided and with whom.
fn reservation_blocks(result: &Value) -> Vec<Block> {
    if result["reserved"].as_bool().unwrap_or(false) {
        return vec![Block::text(granted_summary(result))];
    }

    let conflicts = result["conflicts"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    let mut table = Table::new(vec![
        Column::new("SELECTOR").fixed(),
        Column::new("HELD BY").fixed(),
        Column::new("HOLDER").fixed(),
    ])
    .keep_all_columns()
    .empty_message("denied, but no conflicting holder was reported");
    for conflict in conflicts {
        table.add_row(vec![
            conflict["file"].as_str().unwrap_or_default().to_string(),
            conflict["held_by"].as_str().unwrap_or_default().to_string(),
            conflict["held_by_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        ]);
    }
    vec![
        Block::table(table),
        Block::text(format!(
            "not reserved — {} selector(s) are already held; nothing was claimed",
            conflicts.len()
        )),
    ]
}

fn granted_summary(result: &Value) -> String {
    let files = result["reserved_files"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    let mut out = String::new();
    let reservation_id = result["reservation_id"].as_str().unwrap_or_default();
    match result["expires_at"].as_str() {
        Some(expires_at) => {
            let _ = writeln!(
                out,
                "Reserved {} file(s) until {expires_at} ({reservation_id})",
                files.len()
            );
        }
        None => {
            let _ = writeln!(out, "Reserved {} file(s) ({reservation_id})", files.len());
        }
    }
    for file in files {
        if let Some(file) = file.as_str() {
            let _ = writeln!(out, "  - {file}");
        }
    }
    let _ = write!(
        out,
        "Release it early with `orbit task locks release {reservation_id} --confirm`."
    );
    out
}

/// Hotspot rows rendered by default. Long enough to show a pattern, short
/// enough that the tail of singletons does not bury it.
const DEFAULT_CONTENTION_LIMIT: usize = 10;

#[derive(Args)]
#[command(
    about = "Show which files the pending backlog collides on (what caps parallelism)",
    after_help = "Examples:\n  orbit task locks contention\n  orbit task locks contention --limit 25\n  orbit task locks contention --json\n\n\
                  TASKS counts pending tasks whose lock surface overlaps that selector; a\n\
                  selector claimed by one task constrains nothing and is not listed.\n\n\
                  GROUPS counts clusters of pending tasks linked by overlapping surfaces.\n\
                  Tasks in different groups can never block each other, so the group count\n\
                  is a floor on how many dispatch can admit at once — not a ceiling, since\n\
                  two tasks inside one group are often compatible and merely linked through\n\
                  a third.\n\n\
                  Surfaces are the ones conflict admission reserves: declared selectors,\n\
                  pruned of paths that no longer exist. A task declaring nothing locks\n\
                  nothing and is counted apart."
)]
pub struct LocksContentionArgs {
    /// Maximum hotspot rows to show. Default 10.
    #[arg(long, default_value_t = DEFAULT_CONTENTION_LIMIT)]
    pub limit: usize,
}

impl Execute for LocksContentionArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let report = runtime.task_lock_contention(&[TaskStatus::Backlog])?;

        let mut table = Table::new(vec![
            Column::new("SELECTOR").fixed(),
            Column::new("TASKS").number(),
        ])
        .keep_all_columns();
        for hotspot in report.hotspots.iter().take(self.limit) {
            table.add_row(vec![hotspot.selector.clone(), hotspot.tasks().to_string()]);
        }

        let doc = json!({
            "hotspots": report
                .hotspots
                .iter()
                .map(|hotspot| json!({
                    "selector": hotspot.selector,
                    "tasks": hotspot.tasks(),
                    "task_ids": hotspot.task_ids,
                }))
                .collect::<Vec<_>>(),
            "pending": {
                "total": report.pending(),
                "constrained": report.constrained,
                "unconstrained": report.unconstrained,
            },
            "groups": report.groups,
            "largest_group": report.largest_group,
            "parallel_floor": report.parallel_floor(),
        });

        // With no hotspots the summary line already says why, so an empty grid
        // (and its own "nothing found" message) would only contradict it.
        let mut blocks = Vec::new();
        if !report.hotspots.is_empty() {
            blocks.push(Block::table(table));
        }
        blocks.push(Block::text(contention_summary(&report, self.limit)));
        Ok(Payload::blocks(doc, blocks).into())
    }
}

/// One line stating what the numbers mean for dispatch.
fn contention_summary(report: &orbit_core::LockContentionReport, limit: usize) -> String {
    if report.pending() == 0 {
        return "no pending tasks to analyze".to_string();
    }
    if report.hotspots.is_empty() {
        return format!(
            "{} pending tasks, no contention — no selector is claimed by more than one",
            report.pending()
        );
    }
    let shown = report.hotspots.len().min(limit);
    let elided = if shown < report.hotspots.len() {
        format!(", top {shown} shown")
    } else {
        String::new()
    };
    format!(
        "{} pending tasks ({} declare no files), {} contended selector(s){} — at least {} can run in parallel, largest cluster chains {}",
        report.pending(),
        report.unconstrained,
        report.hotspots.len(),
        elided,
        report.parallel_floor(),
        report.largest_group,
    )
}
