//! The codebase analytics CLI, derived from the codebase operation registry.
//!
//! All verbs call the read-only `orbit.codebase.*` tools, so `--json` is the
//! payload the dashboard's Codebase view reads. What is left here is the text
//! rendering: each ranking's leading rows for `analytics`, and one path's
//! standings with the tasks behind every count for `show`, and co-change
//! measures for `related`.

use std::fmt::Write as _;

use clap::Args;
use orbit_common::governance::codebase::CodebaseVerb;
use orbit_core::OrbitRuntime;
use serde_json::Value;

use super::operation_args::Invocation;
use crate::command::{CommandOut, Execute, Payload};
use crate::output::payload::Block;
use crate::output::table::{Column, Table};

/// Rows of each ranking the text view shows; `--json` carries every row the
/// payload ranks.
const TEXT_ROWS: usize = 10;

/// A path ranking as the text views name it.
struct Ranking {
    key: &'static str,
    label: &'static str,
    /// What the numerator counts.
    numerator: &'static str,
    /// What the denominator counts.
    denominator: &'static str,
}

/// The path rankings in the dashboard's order.
const PATH_RANKINGS: &[Ranking] = &[
    Ranking {
        key: "regressions",
        label: "Regressions",
        numerator: "done tasks a later task regressed",
        denominator: "done tasks that declared it",
    },
    Ranking {
        key: "footprint_misses",
        label: "Footprint misses",
        numerator: "tasks that widened onto it mid-run",
        denominator: "tasks that declared it",
    },
    Ranking {
        key: "blocked",
        label: "Blocked by a run",
        numerator: "tasks a run failure blocked",
        denominator: "tasks that declared it",
    },
    Ranking {
        key: "rejected",
        label: "Rejected after a run",
        numerator: "tasks rejected after a run",
        denominator: "tasks with a run that declared it",
    },
    Ranking {
        key: "lock_contention",
        label: "Lock contention",
        numerator: "requesters refused a lock on it",
        denominator: "requesters that reserved or were refused it",
    },
];

#[derive(Args)]
#[command(about = orbit_common::governance::codebase::CODEBASE.cli_about)]
pub struct CodebaseCommand {
    #[command(subcommand)]
    pub command: Invocation<CodebaseVerb>,
}

impl Execute for CodebaseCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let Invocation { spec, input } = self.command;
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
        let blocks = match spec.verb {
            CodebaseVerb::Analytics => analytics_blocks(&value),
            CodebaseVerb::Show => show_blocks(&value),
            CodebaseVerb::Related => related_blocks(&value),
        };
        Ok(Payload::blocks(value, blocks).into())
    }
}

fn analytics_blocks(value: &Value) -> Vec<Block> {
    let mut blocks = vec![Block::text(selection_line(value))];

    let overlap = &value["overlap"];
    blocks.push(Block::text(format!(
        "\nIn several rankings: {} of {} ranked paths",
        rows(overlap).len(),
        count(&overlap["ranked_paths"]),
    )));
    let mut table = Table::new(vec![
        Column::new("PATH").path(),
        Column::new("RANKINGS"),
        Column::new("TOP RATE").number(),
    ])
    .empty_message("no path is in more than one ranking");
    for row in rows(overlap).iter().take(TEXT_ROWS) {
        let rankings = row["rankings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(ranking_label)
            .collect::<Vec<_>>()
            .join(", ");
        table.add_row(vec![
            text(&row["path"]),
            rankings,
            percent(&row["top_rate"]),
        ]);
    }
    blocks.push(Block::table(table));

    for ranking in PATH_RANKINGS {
        let section = &value[ranking.key];
        blocks.push(Block::text(format!(
            "\n{}: {} ÷ {} ({} below minimum support)",
            ranking.label,
            ranking.numerator,
            ranking.denominator,
            count(&section["below_support"]),
        )));
        let mut table = rate_table("PATH");
        for (index, row) in rows(section).iter().take(TEXT_ROWS).enumerate() {
            table.add_row(rate_row(index, text(&row["path"]), row));
        }
        blocks.push(Block::table(table));
    }

    let coupling = &value["change_coupling"];
    blocks.push(Block::text(format!(
        "\nChange coupling: reserved together ÷ reserved either ({} pairs below minimum support)",
        count(&coupling["below_support"]),
    )));
    let mut table = rate_table("PATHS");
    for (index, row) in rows(coupling).iter().take(TEXT_ROWS).enumerate() {
        let paths = row["paths"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" <-> ");
        table.add_row(rate_row(index, paths, row));
    }
    blocks.push(Block::table(table));

    blocks.push(Block::text(format!(
        "\nShowing up to {TEXT_ROWS} rows per ranking; --json carries up to {}. \
         Follow a path to its tasks with `orbit codebase show <path>`.",
        count(&value["row_limit"]),
    )));
    blocks
}

fn show_blocks(value: &Value) -> Vec<Block> {
    let rankings = &value["rankings"];
    let mut blocks = vec![Block::text(format!(
        "{}\n{}",
        text(&value["path"]),
        selection_line(value)
    ))];

    let mut table = Table::new(vec![
        Column::new("RANKING").fixed(),
        Column::new("RANK").fixed(),
        Column::new("RATE").number(),
        Column::new("COUNT").number(),
    ]);
    for ranking in PATH_RANKINGS {
        let row = &rankings[ranking.key];
        if row.is_null() {
            table.add_row(vec![
                ranking.label.to_string(),
                "-".into(),
                "-".into(),
                "-".into(),
            ]);
            continue;
        }
        let rank = match row["rank"].as_u64() {
            Some(rank) => format!("#{rank}"),
            None if row["below_support"] == true => "below support".into(),
            None => "-".into(),
        };
        table.add_row(vec![
            ranking.label.to_string(),
            rank,
            percent(&row["rate"]),
            fraction(row),
        ]);
    }
    blocks.push(Block::table(table));

    let mut out = String::new();
    for ranking in PATH_RANKINGS {
        let row = &rankings[ranking.key];
        if ranking.key == "lock_contention" {
            let contenders = strings(&row["contenders"]);
            if !contenders.is_empty() {
                let _ = writeln!(
                    out,
                    "\n{}: {} ({})\n  {}",
                    ranking.label,
                    ranking.numerator,
                    contenders.len(),
                    contenders.join(", ")
                );
            }
            continue;
        }
        write_tasks(&mut out, ranking.label, ranking.numerator, &row["tasks"]);
        if ranking.key == "blocked" {
            write_tasks(
                &mut out,
                ranking.label,
                "tasks blocked with no cause found",
                &row["unclassified_tasks"],
            );
        }
    }

    let coupling = &rankings["change_coupling"];
    let pairs = coupling["pairs"].as_array().cloned().unwrap_or_default();
    let _ = writeln!(
        out,
        "\nChange coupling: {} pairs at minimum support (reserved {} times)",
        pairs.len(),
        count(&coupling["reservations"]),
    );
    let path = text(&value["path"]);
    for pair in &pairs {
        let other = strings(&pair["paths"])
            .into_iter()
            .find(|candidate| *candidate != path)
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "  #{} {other}  {} ({})",
            count(&pair["rank"]),
            percent(&pair["rate"]),
            fraction(pair),
        );
        let requesters = strings(&pair["requesters"]);
        if !requesters.is_empty() {
            let _ = writeln!(out, "    reserved together by {}", requesters.join(", "));
        }
    }
    blocks.push(Block::text(out));
    blocks
}

/// One ranking's numerator tasks, newest first, one per line; nothing when
/// there are none.
fn write_tasks(out: &mut String, label: &str, numerator: &str, tasks: &Value) {
    let tasks = tasks.as_array().cloned().unwrap_or_default();
    if tasks.is_empty() {
        return;
    }
    let _ = writeln!(out, "\n{label}: {numerator} ({})", tasks.len());
    for task in &tasks {
        let at = text(&task["at"]);
        let day = at.get(..10).unwrap_or(&at);
        let _ = write!(
            out,
            "  {}  {day}  {}",
            text(&task["task_id"]),
            text(&task["title"])
        );
        let regressions = strings(&task["regression_tasks"]);
        if !regressions.is_empty() {
            let _ = write!(out, "  (regressed by {})", regressions.join(", "));
        }
        out.push('\n');
    }
}

fn selection_line(value: &Value) -> String {
    format!(
        "window {} · minimum support {} · grouped by {} · {} tasks scanned · computed {}",
        text(&value["window"]),
        count(&value["min_support"]),
        text(&value["rollup"]),
        count(&value["tasks_scanned"]),
        text(&value["computed_at"]),
    )
}

fn rate_table(subject: &str) -> Table {
    Table::new(vec![
        Column::new("#").number(),
        Column::new(subject).path(),
        Column::new("RATE").number(),
        Column::new("COUNT").number(),
    ])
    .empty_message("nothing reaches the minimum support in this window")
}

fn rate_row(index: usize, subject: String, row: &Value) -> Vec<String> {
    vec![
        (index + 1).to_string(),
        subject,
        percent(&row["rate"]),
        fraction(row),
    ]
}

fn ranking_label(key: &str) -> String {
    PATH_RANKINGS
        .iter()
        .find(|ranking| ranking.key == key)
        .map_or(key, |ranking| ranking.label)
        .to_string()
}

fn rows(section: &Value) -> Vec<Value> {
    section["rows"].as_array().cloned().unwrap_or_default()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

fn count(value: &Value) -> String {
    value.as_u64().unwrap_or(0).to_string()
}

fn fraction(row: &Value) -> String {
    format!(
        "{}/{}",
        count(&row["numerator"]),
        count(&row["denominator"])
    )
}

/// A rate as the dashboard shows it: whole percent, one decimal under 10%.
fn percent(rate: &Value) -> String {
    let rate = rate.as_f64().unwrap_or(0.0) * 100.0;
    if rate > 0.0 && rate < 10.0 {
        format!("{rate:.1}%")
    } else {
        format!("{rate:.0}%")
    }
}

fn related_blocks(value: &Value) -> Vec<Block> {
    let mut table = Table::new(vec![
        Column::new("PATH").path(),
        Column::new("JACCARD").number(),
        Column::new("CONDITIONAL").number(),
        Column::new("SHARED").number(),
        Column::new("RESERVATION").number(),
        Column::new("MISSES").number(),
    ])
    .empty_message("no partners reach minimum shared support");
    for row in value["rows"].as_array().into_iter().flatten() {
        table.add_row(vec![
            text(&row["path"]),
            percent(&row["jaccard"]),
            percent(&row["conditional_rate"]),
            count(&row["support"]["both"]),
            if row["reservation_coupling"].is_null() {
                "—".into()
            } else {
                percent(&row["reservation_coupling"]["rate"])
            },
            count(&row["footprint_miss_count"]),
        ]);
    }
    vec![
        Block::text(format!(
            "{}\n{}\n{} bulk tasks excluded; {} done tasks lack readable landed evidence",
            text(&value["path"]),
            selection_line(value),
            count(&value["excluded_large"]),
            count(&value["unavailable_landed_tasks"])
        )),
        Block::table(table),
    ]
}
