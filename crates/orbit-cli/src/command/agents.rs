//! The agent scorecard CLI, derived from the agents operation registry.
//!
//! Both verbs call the read-only `orbit.agents.*` tools, so `--json` is the
//! payload the dashboard's Agents view reads. What is left here is the text
//! rendering, shared with the deprecated `orbit orchestrator` aliases: one
//! row per crew and model for `scorecard`, and one row's terms with the tasks
//! behind every count for `show`.
//!
//! `vision-probe` is hand-wired: it launches one canary run per crew and
//! prints whether each model returned the code.

use std::fmt::Write as _;

use clap::{Args, Subcommand};
use orbit_common::governance::agents::AgentsVerb;
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::Value;

use super::operation_args::Invocation;
use crate::command::{CommandOut, Execute, Payload};
use crate::output::payload::Block;
use crate::output::table::{Column, Table};

#[derive(Args)]
#[command(about = orbit_common::governance::agents::AGENTS.cli_about)]
pub struct AgentsCommand {
    #[command(subcommand)]
    pub command: AgentsSubcommand,
}

#[derive(Subcommand)]
pub enum AgentsSubcommand {
    // Scorecard and show, derived from the agents operation registry.
    #[command(flatten)]
    Operation(Invocation<AgentsVerb>),
    /// Check that each crew's model sees images: run the canary per crew
    #[command(long_about = VISION_PROBE_LONG_ABOUT)]
    VisionProbe(AgentsVisionProbeArgs),
}

const VISION_PROBE_LONG_ABOUT: &str = "Check that each crew's model sees images.

Runs one canary per crew outside any delivery: a generated PNG of a random \
six-character code is staged for the crew's executor through its \
spec.image_input declaration, and the agent must return the code. Prints one \
row per crew: its executor, the declared mode, and the result (seen, \
image_not_seen, no_image_input, vision_off, no_cli_runtime, failed or \
timeout), plus any visual hold two consecutive misses placed on it.

Without --crew, every enabled crew is probed and a crew that cannot take \
images is reported as a row. A crew named with --crew that cannot take images \
fails the command before anything launches.";

#[derive(Args)]
pub struct AgentsVisionProbeArgs {
    /// Probe only this crew (repeatable)
    #[arg(long = "crew", value_name = "NAME")]
    pub crews: Vec<String>,
    /// Seconds to wait for the probe runs to settle
    #[arg(long, value_name = "SECONDS", default_value_t = 900)]
    pub timeout_seconds: u64,
}

impl Execute for AgentsCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let invocation = match self.command {
            AgentsSubcommand::Operation(invocation) => invocation,
            AgentsSubcommand::VisionProbe(args) => {
                let report = runtime.vision_probe(&args.crews, args.timeout_seconds)?;
                let value = serde_json::to_value(&report)
                    .map_err(|error| OrbitError::Execution(error.to_string()))?;
                let blocks = vision_probe_blocks(&value);
                return Ok(Payload::blocks(value, blocks).into());
            }
        };
        let Invocation { spec, input } = invocation;
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
        let blocks = match spec.verb {
            AgentsVerb::Scorecard if value["per_task"] == true => per_task_blocks(&value),
            AgentsVerb::Scorecard if value["shadow"] == true => shadow_blocks(&value),
            AgentsVerb::Scorecard if value["by_type"] == true => by_type_blocks(&value),
            AgentsVerb::Scorecard => scorecard_blocks(&value),
            AgentsVerb::Show => show_blocks(&value),
        };
        Ok(Payload::blocks(value, blocks).into())
    }
}

/// One row per crew and model, ranked rows first, with a column per term.
///
/// The selection line and the units footer are status, not records, so they
/// are notices: stderr in every mode, which keeps stdout to the rows alone.
pub(crate) fn scorecard_blocks(value: &Value) -> Vec<Block> {
    let terms = value["terms"].as_array().cloned().unwrap_or_default();
    let computed = value["status"] != "not_computed";
    let empty = if computed {
        "no row in the window".to_string()
    } else {
        format!(
            "The {} scorecard is not computed yet.",
            text(&value["role"])
        )
    };
    let mut columns = vec![
        Column::new("RANK").number(),
        Column::new("CREW").fixed(),
        Column::new("MODEL"),
        Column::new("EFFORT"),
        Column::new("UNITS").number(),
        Column::new("SCORE").number(),
    ];
    columns.extend(
        terms
            .iter()
            .map(|term| Column::new(&text(&term["label"]).to_uppercase()).number()),
    );
    let mut table = Table::new(columns)
        .keep_all_columns()
        .empty_message(empty)
        .trailing_notice(selection_line(value));
    for row in value["rows"].as_array().into_iter().flatten() {
        let mut cells = vec![
            if row["unjudged"] == true {
                "unjudged".into()
            } else if row["below_support"] == true {
                "below".into()
            } else {
                number(&row["rank"])
            },
            text(&row["crew"]),
            text(&row["model"]),
            effort_labels(&row["effort_breakdown"]),
            number(&row["units"]),
            number(&row["score"]),
        ];
        cells.extend(
            terms
                .iter()
                .map(|term| number(&row["counts"][text(&term["key"])])),
        );
        table.add_row(cells);
    }
    if computed {
        let formula = if value["role"] == "implementers" {
            "100 + weighted (actual − expected) per 100 tasks; expectations use the fleet's rates for the same tier mix; tasks tagged no-diff-expected when an attempt was admitted are not implementer work and count nowhere here"
        } else {
            "each term per 100 units, weighted and summed"
        };
        table = table.trailing_notice(format!(
            "Units: {}. Score: {formula}; a row above the fleet average ({}) is pulled down toward it.\nWeights: {}",
            text(&value["unit"]),
            number(&value["fleet_average"]),
            terms
                .iter()
                .map(|term| format!("{} × {}", number(&term["weight"]), text(&term["key"])))
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    vec![Block::table(table)]
}

/// The shadow share report: one row per crew in a pool and type, then the
/// draws that recorded no performance factor.
fn shadow_blocks(value: &Value) -> Vec<Block> {
    let mut recorded = Table::new(vec![
        Column::new("POOL").fixed(),
        Column::new("TYPE"),
        Column::new("CREW").fixed(),
        Column::new("DRAWS").number(),
        Column::new("STATIC").number(),
        Column::new("DRAWN").number(),
        Column::new("WOULD-BE").number(),
    ])
    .keep_all_columns()
    .empty_message("no recorded draw in the window".to_string());
    for pool in value["pools"].as_array().into_iter().flatten() {
        for crew in pool["crews"].as_array().into_iter().flatten() {
            recorded.add_row(vec![
                text(&pool["pool"]),
                text(&pool["type"]),
                text(&crew["crew"]),
                number(&crew["draws"]),
                percent(&crew["static_share"]),
                percent(&crew["drawn_share"]),
                percent(&crew["would_be_share"]),
            ]);
        }
    }
    let mut unrecorded = Table::new(vec![
        Column::new("POOL").fixed(),
        Column::new("CREW").fixed(),
        Column::new("DRAWS").number(),
        Column::new("STATIC").number(),
        Column::new("DRAWN").number(),
    ])
    .keep_all_columns()
    .empty_message("No draws without a performance record in the window.".to_string());
    for pool in value["static_draws"]["pools"]
        .as_array()
        .into_iter()
        .flatten()
    {
        for crew in pool["crews"].as_array().into_iter().flatten() {
            unrecorded.add_row(vec![
                text(&pool["pool"]),
                text(&crew["crew"]),
                number(&crew["draws"]),
                percent(&crew["static_share"]),
                percent(&crew["drawn_share"]),
            ]);
        }
    }
    vec![
        Block::text(
            "Shares are the mean of each draw's weight share. Would-be applies the recorded factor the draw did not use. Draws without a performance record are listed separately and counted as static."
                .to_string(),
        ),
        Block::table(recorded),
        Block::text("Draws without a performance record".to_string()),
        Block::table(unrecorded),
    ]
}

/// The by-type matrix: one row per (crew, model, tier, type) cell.
pub(crate) fn by_type_blocks(value: &Value) -> Vec<Block> {
    let columns = vec![
        Column::new("CREW").fixed(),
        Column::new("MODEL"),
        Column::new("TIER"),
        Column::new("TYPE"),
        Column::new("N").number(),
        Column::new("RAW").number(),
        Column::new("CREW MEAN").number(),
        Column::new("SCORE").number(),
    ];
    let types = value["types"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| text(&entry["name"]))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let mut table = Table::new(columns)
        .keep_all_columns()
        .empty_message("no row in the window".to_string())
        .trailing_notice(selection_line(value))
        .trailing_notice(format!(
            "Types: {}; tasks matching none are other. Score: two-level shrinkage toward the crew mean, then toward 100 (K = 30).",
            if types.is_empty() { "none configured".to_string() } else { types },
        ));
    for cell in value["cells"].as_array().into_iter().flatten() {
        table.add_row(vec![
            text(&cell["crew"]),
            text(&cell["model"]),
            text(&cell["tier"]),
            text(&cell["type"]),
            number(&cell["n"]),
            number(&cell["raw"]),
            number(&cell["crew_mean"]),
            number(&cell["score"]),
        ]);
    }
    vec![Block::table(table)]
}

/// The per-task export: one row per implemented task, without naming it.
/// Use `--json` for the machine-readable export the offline report reads.
fn per_task_blocks(value: &Value) -> Vec<Block> {
    let terms = value["terms"].as_array().cloned().unwrap_or_default();
    let mut columns = vec![
        Column::new("CREW").fixed(),
        Column::new("MODEL"),
        Column::new("TIER"),
        Column::new("TYPE"),
        Column::new("TAGS"),
        Column::new("PATHS"),
    ];
    columns.extend(
        terms
            .iter()
            .map(|term| Column::new(&text(&term["label"]).to_uppercase()).number()),
    );
    let mut table = Table::new(columns)
        .keep_all_columns()
        .empty_message("no implemented task in the window".to_string())
        .trailing_notice(selection_line(value))
        .trailing_notice(
            "One row per implemented task; use --json for the machine-readable export.".to_string(),
        );
    for task in value["tasks"].as_array().into_iter().flatten() {
        let join = |key: &str| {
            task[key]
                .as_array()
                .map(|items| items.iter().map(text).collect::<Vec<_>>().join(","))
                .unwrap_or_default()
        };
        let mut cells = vec![
            text(&task["crew"]),
            text(&task["model"]),
            text(&task["tier"]),
            text(&task["type"]),
            join("tags"),
            join("path_prefixes"),
        ];
        cells.extend(
            terms
                .iter()
                .map(|term| number(&task["counts"][text(&term["key"])])),
        );
        table.add_row(cells);
    }
    vec![Block::table(table)]
}

/// One row's terms, then the tasks behind each nonzero count.
pub(crate) fn show_blocks(value: &Value) -> Vec<Block> {
    let row = &value["row"];
    let standing = if row["unjudged"] == true {
        "unjudged: no unit judged yet".to_string()
    } else if row["below_support"] == true {
        "below minimum support".to_string()
    } else {
        format!("rank {}", number(&row["rank"]))
    };
    let mut blocks = vec![Block::text(format!(
        "{} · {} ({standing})\n{}\n{} {} · score {} · fleet average {}",
        text(&row["crew"]),
        text(&row["model"]),
        selection_line(value),
        number(&row["units"]),
        text(&value["unit"]),
        number(&row["score"]),
        number(&value["fleet_average"]),
    ))];

    blocks.push(Block::text(format!(
        "Recorded effort: {}",
        effort_labels(&row["effort_breakdown"])
    )));

    let terms = value["terms"].as_array().cloned().unwrap_or_default();
    let normalised = row["expected"].is_object();
    if normalised {
        blocks.push(Block::text(format!("Raw score starts at {}; contributions compare actual with expected counts for the same tier mix.", number(&row["score_base"]))));
    }
    let mut columns = vec![
        Column::new("TERM").fixed(),
        Column::new("WEIGHT").number(),
        Column::new("COUNT").number(),
        Column::new("PER 100").number(),
        Column::new("CONTRIBUTION").number(),
    ];
    if normalised {
        columns.insert(3, Column::new("EXPECTED").number());
    }
    let mut table = Table::new(columns).keep_all_columns();
    for term in &terms {
        let mut cells = vec![
            text(&term["label"]),
            number(&term["weight"]),
            number(&term["count"]),
            number(&term["per_100"]),
            number(&term["contribution"]),
        ];
        if normalised {
            cells.insert(3, number(&row["expected"][text(&term["key"])]));
        }
        table.add_row(cells);
    }
    blocks.push(Block::table(table));

    let mut out = String::new();
    for term in &terms {
        let tasks = term["tasks"].as_array().cloned().unwrap_or_default();
        if tasks.is_empty() {
            continue;
        }
        let _ = writeln!(
            out,
            "\n{} ({})",
            text(&term["label"]),
            number(&term["count"])
        );
        for task in &tasks {
            let _ = writeln!(
                out,
                "  {}  {}  {}",
                text(&task["task_id"]),
                number(&task["count"]),
                text(&task["title"])
            );
            for occurrence in task["occurrences"].as_array().into_iter().flatten() {
                let at = text(&occurrence["at"]);
                let _ = writeln!(
                    out,
                    "    {}  {}",
                    at.get(..16).unwrap_or(&at),
                    text(&occurrence["detail"])
                );
            }
        }
    }
    if !out.is_empty() {
        blocks.push(Block::text(out));
    }
    blocks
}

/// One row per probed crew: executor, mode, result and any visual hold.
fn vision_probe_blocks(value: &Value) -> Vec<Block> {
    let mut table = Table::new(vec![
        Column::new("CREW").fixed(),
        Column::new("EXECUTOR"),
        Column::new("MODEL"),
        Column::new("MODE"),
        Column::new("RESULT"),
        Column::new("HOLD"),
        Column::new("DETAIL"),
    ])
    .keep_all_columns()
    .empty_message("no crew to probe".to_string());
    for row in value["rows"].as_array().into_iter().flatten() {
        let hold = &row["hold"];
        table.add_row(vec![
            text(&row["crew"]),
            text(&row["executor"]),
            text(&row["model"]),
            row["mode"].as_str().unwrap_or("-").to_string(),
            text(&row["result"]),
            if hold.is_object() {
                format!(
                    "{} misses, until {}",
                    number(&hold["consecutive_misses"]),
                    text(&hold["until"])
                )
            } else {
                "-".to_string()
            },
            row["detail"].as_str().unwrap_or("").to_string(),
        ]);
    }
    vec![Block::table(table)]
}

fn selection_line(value: &Value) -> String {
    // Only implementer counts read the execution scope; the shadow report,
    // which reads draws, carries none.
    let execution = match value["execution"].as_str() {
        Some(scope) if value["role"] == "implementers" => format!(" · {scope} tasks"),
        _ => String::new(),
    };
    format!(
        "{} · window {} · minimum support {}{execution} · {} tasks scanned · computed {}",
        text(&value["role"]),
        text(&value["window"]),
        number(&value["min_support"]),
        number(&value["tasks_scanned"]),
        text(&value["computed_at"]),
    )
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

/// A share as a percent to one place. `number` collapses `0.5102` to `0.5`.
fn percent(value: &Value) -> String {
    value
        .as_f64()
        .map_or_else(|| "-".to_string(), |share| format!("{:.1}%", share * 100.0))
}

/// A count, rate or score; `-` when the payload has none.
fn number(value: &Value) -> String {
    match value {
        Value::Number(number) if number.is_f64() => {
            let rate = number.as_f64().unwrap_or(0.0);
            if rate.fract() == 0.0 {
                format!("{rate:.0}")
            } else {
                format!("{rate:.1}")
            }
        }
        Value::Number(number) => number.to_string(),
        _ => "-".into(),
    }
}

fn effort_labels(value: &Value) -> String {
    let labels = value
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| {
            if row["reasoning_effort"].is_null() {
                "provider default".to_string()
            } else {
                text(&row["reasoning_effort"])
            }
        })
        .collect::<Vec<_>>();
    if labels.is_empty() {
        "unknown".into()
    } else {
        labels.join(", ")
    }
}
