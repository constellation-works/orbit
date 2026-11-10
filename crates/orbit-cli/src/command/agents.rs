//! The agent scorecard CLI, derived from the agents operation registry.
//!
//! The verbs call the read-only `orbit.agents.*` tools, so `--json` is the
//! payload the dashboard's Agents view reads. What is left here is the text
//! rendering: one row per crew and model for `scorecard`, one row's terms
//! with the tasks behind every count for `show`, and one row per recorded
//! required-check failure for `ci-failures`.
//!
//! `vision-probe` is hand-wired: it launches one canary run per crew and
//! prints whether each model returned the code. `rank` is hand-wired too:
//! the model ranking is a CLI and dashboard read, and `rank calibrate`
//! writes the workspace's noise-scale seasons.

use std::fmt::Write as _;

use clap::{Args, Subcommand};
use orbit_common::governance::agents::AgentsVerb;
use orbit_core::application::agents::RankQuery;
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
    /// Rank implementer models by quality and reliability, in tie bands
    #[command(long_about = RANK_LONG_ABOUT)]
    Rank(AgentsRankArgs),
    /// Check that each crew's model sees images: run the canary per crew
    #[command(long_about = VISION_PROBE_LONG_ABOUT)]
    VisionProbe(AgentsVisionProbeArgs),
}

const RANK_LONG_ABOUT: &str = "Rank implementer models by quality and reliability, in tie bands.

One row per model, its crews and effort levels pooled. Each measure (escaped \
regressions, review outcome, implementer CI failures, implementer-fault final \
recoveries, crew-caused rework, tool-call errors) is compared with what the \
fleet achieves on the same task tiers, divided by the season's noise scale, \
weighted, and shrunk toward the other eligible models. Measures Orbit does not \
record yet are shown as unmeasured and leave the weights.

The ranked window (--window, 30 days) ends --settle-lag (14 days) before \
--until, so late escaped regressions have arrived; --live ends it at --until \
instead and is provisional. A model needs at least 30 implemented tasks \
and at least one assessed tier with at least 5 tasks to be \
ranked, and the board needs three such models. A ranked model spanning \
exactly one assessed tier is flagged 'single tier: <tier>'. Models \
whose scores are not distinguishable share a band, cheapest first.

Use 'orbit agents rank calibrate' to freeze a new season's noise scales in the \
workspace's state; until then the scales are estimated from the ranked window.";

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct AgentsRankArgs {
    #[command(subcommand)]
    pub action: Option<AgentsRankAction>,
    #[command(flatten)]
    pub selection: AgentsRankSelection,
    /// Rank the window ending at --until, unsettled and provisional
    #[arg(long)]
    pub live: bool,
}

#[derive(Subcommand)]
pub enum AgentsRankAction {
    /// Freeze a new season's noise scales from the settled window
    Calibrate(AgentsRankSelection),
}

#[derive(Args)]
pub struct AgentsRankSelection {
    /// Time window, such as 7d or 30d [default: 30d]
    #[arg(long, value_name = "WINDOW")]
    pub window: Option<String>,
    /// End before the settle lag: an RFC 3339 instant or a duration ago such as 7d [default: now]
    #[arg(long, value_name = "WHEN")]
    pub until: Option<String>,
    /// How long before --until the ranked window ends [default: 14d]
    #[arg(long, value_name = "DURATION")]
    pub settle_lag: Option<String>,
    /// Which tasks count by derived execution: managed, all or interactive [default: managed]
    #[arg(long, value_name = "SCOPE")]
    pub execution: Option<String>,
}

impl AgentsRankSelection {
    fn query(&self, live: bool) -> Result<RankQuery, OrbitError> {
        RankQuery::parse(
            self.window.as_deref(),
            self.until.as_deref(),
            self.settle_lag.as_deref(),
            self.execution.as_deref(),
            live,
        )
    }
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
            AgentsSubcommand::Rank(args) => {
                let (value, blocks) = match &args.action {
                    None => {
                        let value = runtime.agent_rank(&args.selection.query(args.live)?)?;
                        let blocks = rank_blocks(&value);
                        (value, blocks)
                    }
                    Some(AgentsRankAction::Calibrate(selection)) => {
                        let value = runtime.agent_rank_calibrate(&selection.query(false)?)?;
                        let blocks = calibrate_blocks(&value);
                        (value, blocks)
                    }
                };
                return Ok(Payload::blocks(value, blocks).into());
            }
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
            AgentsVerb::Scorecard if value["by_type"] == true => by_type_blocks(&value),
            AgentsVerb::Scorecard => scorecard_blocks(&value),
            AgentsVerb::Show => show_blocks(&value),
            AgentsVerb::CiFailures => ci_failure_blocks(&value),
        };
        Ok(Payload::blocks(value, blocks).into())
    }
}

/// One row per crew and model, ranked rows first, with a column per term.
///
/// The selection line and the units footer are status, not records, so they
/// are notices: stderr in every mode, which keeps stdout to the rows alone.
fn scorecard_blocks(value: &Value) -> Vec<Block> {
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
        Column::new("ROW"),
        Column::new("CREWS"),
        Column::new("UNITS").number(),
        Column::new("SCORE").number(),
        Column::new("P10-P90"),
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
        let interval = &row["interval"];
        let mut cells = vec![
            if row["unjudged"] == true {
                "unjudged".into()
            } else if row["below_support"] == true {
                "below".into()
            } else {
                number(&row["rank"])
            },
            text(&row["label"]),
            row["crews"]
                .as_array()
                .map(|crews| crews.iter().map(text).collect::<Vec<_>>().join(", "))
                .unwrap_or_else(|| text(&row["crew"])),
            number(&row["units"]),
            number(&row["score"]),
            if interval.is_object() {
                format!("{}-{}", number(&interval["p10"]), number(&interval["p90"]))
            } else {
                "-".to_string()
            },
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
            "Units: {}. Score: {formula}; a row above the fleet average ({}) is pulled down toward it. P10-P90 is the score's 10th to 90th percentile over 1,000 resamples of the row's tasks; ranked rows only.\nWeights: {}",
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

/// One row per recorded required-check failure, newest first, with the
/// per-class counts as a notice.
pub(crate) fn ci_failure_blocks(value: &Value) -> Vec<Block> {
    let columns = vec![
        Column::new("RECORDED"),
        Column::new("TASK").fixed(),
        Column::new("CLASS"),
        Column::new("HEAD").fixed(),
        Column::new("CHECK"),
        Column::new("SIGNATURE"),
    ];
    let mut table = Table::new(columns).empty_message("no recorded CI failure in the window");
    for record in value["records"].as_array().into_iter().flatten() {
        let class = text(&record["class"]);
        let recorded = text(&record["recorded_class"]);
        table.add_row(vec![
            text(&record["recorded_at"]),
            text(&record["task_id"]),
            if class == recorded {
                class
            } else {
                format!("{class} (was {recorded})")
            },
            text(&record["head_sha"]).chars().take(12).collect(),
            text(&record["check"]),
            text(&record["signature"]),
        ]);
    }
    let counts = ["code", "flaky", "infrastructure", "unknown"]
        .iter()
        .map(|class| format!("{class} {}", number(&value["counts"][*class])))
        .collect::<Vec<_>>()
        .join(", ");
    table = table.trailing_notice(format!("Window: {}. {counts}.", text(&value["window"])));
    vec![Block::table(table)]
}

/// The by-type matrix: one row per (model, recorded effort, tier, type)
/// cell, with the crews that contributed to it.
pub(crate) fn by_type_blocks(value: &Value) -> Vec<Block> {
    let columns = vec![
        Column::new("CREWS").fixed(),
        Column::new("ROW"),
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
            cell["crews"]
                .as_array()
                .map(|crews| crews.iter().map(text).collect::<Vec<_>>().join(", "))
                .unwrap_or_else(|| text(&cell["crew"])),
            {
                let label = text(&cell["label"]);
                if label.is_empty() {
                    text(&cell["model"])
                } else {
                    label
                }
            },
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
        Column::new("CREWS").fixed(),
        Column::new("ROW"),
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
            task["crews"]
                .as_array()
                .map(|crews| crews.iter().map(text).collect::<Vec<_>>().join(", "))
                .unwrap_or_else(|| text(&task["crew"])),
            {
                let label = text(&task["label"]);
                if label.is_empty() {
                    text(&task["model"])
                } else {
                    label
                }
            },
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
fn show_blocks(value: &Value) -> Vec<Block> {
    let row = &value["row"];
    let standing = if row["unjudged"] == true {
        "unjudged: no unit judged yet".to_string()
    } else if row["below_support"] == true {
        "below minimum support".to_string()
    } else {
        format!("rank {}", number(&row["rank"]))
    };
    let mut blocks = vec![Block::text(format!(
        "{} ({standing}) · crews {}\n{}\n{} {} · score {} · fleet average {}",
        text(&row["label"]),
        row["crews"]
            .as_array()
            .map(|crews| crews.iter().map(text).collect::<Vec<_>>().join(", "))
            .unwrap_or_else(|| text(&row["crew"])),
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

/// One row per model in board order, a column per recorded measure's
/// contribution, and notices for the selection, scales and unmeasured
/// measures.
fn rank_blocks(value: &Value) -> Vec<Block> {
    let measures: Vec<Value> = value["measures"].as_array().cloned().unwrap_or_default();
    let recorded: Vec<&Value> = measures
        .iter()
        .filter(|measure| measure["status"] != "unmeasured")
        .collect();
    let mut columns = vec![
        Column::new("RANK").number(),
        Column::new("MODEL"),
        Column::new("TASKS").number(),
        Column::new("SCORE").number(),
        Column::new("P10-P90"),
        Column::new("COST RATIO").number(),
    ];
    columns.extend(
        recorded
            .iter()
            .map(|measure| Column::new(&text(&measure["label"]).to_uppercase()).number()),
    );
    let provisional = if value["live"] == true {
        " · live, provisional".to_string()
    } else {
        format!(" · settle lag {}", text(&value["settle_lag"]))
    };
    let season = match value["season"]["season"].as_u64() {
        Some(season) => format!("season {season} scales"),
        None => "scales estimated from this window (no season calibrated)".to_string(),
    };
    let mut table = Table::new(columns)
        .keep_all_columns()
        .empty_message("no implementer model in the window".to_string())
        .trailing_notice(format!(
            "implementer models · {} to {}{provisional} · {} tasks · {season} · computed {}",
            text(&value["since"]),
            text(&value["until"]),
            text(&value["execution"]),
            text(&value["computed_at"]),
        ));
    for row in value["rows"].as_array().into_iter().flatten() {
        let interval = &row["interval"];
        let mut cells = vec![
            if row["eligible"] != true {
                "-".to_string()
            } else {
                row["rank_label"].as_str().unwrap_or("-").to_string()
            },
            match row["single_tier"].as_str() {
                Some(tier) => format!("{} (single tier: {tier})", text(&row["model"])),
                None => text(&row["model"]),
            },
            number(&row["units"]),
            number(&row["score"]),
            if interval.is_object() {
                format!("{}-{}", number(&interval["p10"]), number(&interval["p90"]))
            } else {
                "-".to_string()
            },
            if row["cost"]["cost_unknown"] == true {
                "unknown".to_string()
            } else {
                number(&row["cost"]["ratio"])
            },
        ];
        cells.extend(
            recorded
                .iter()
                .map(|measure| number(&row["measures"][text(&measure["key"])]["contribution"])),
        );
        table.add_row(cells);
    }
    let unmeasured: Vec<String> = measures
        .iter()
        .filter(|measure| measure["status"] == "unmeasured")
        .map(|measure| format!("{} ({})", text(&measure["label"]), text(&measure["reason"])))
        .collect();
    if !unmeasured.is_empty() {
        table = table.trailing_notice(format!(
            "Unmeasured, outside the weights: {}",
            unmeasured.join("; ")
        ));
    }
    table = table.trailing_notice(format!(
        "Score: 100 + 10 × weighted noise-unit residuals against the same-mix expectation, shrunk toward the other eligible models (K = 30); measure columns are score points before shrinkage.{}",
        if value["qualified"] == true {
            String::new()
        } else {
            format!(
                " Board not qualified: {} eligible models of 3 needed, so no bands or ranks.",
                number(&value["eligible_models"])
            )
        }
    ));
    if value["unattributed"]["tasks"].as_u64().unwrap_or(0) > 0 {
        table = table.trailing_notice(format!(
            "Unattributed: {} tasks whose runs recorded no model count in the fleet expectation only.",
            number(&value["unattributed"]["tasks"])
        ));
    }
    vec![Block::table(table)]
}

/// The calibrated season: its window, reference size and scales.
fn calibrate_blocks(value: &Value) -> Vec<Block> {
    let season = &value["season"];
    let mut table = Table::new(vec![
        Column::new("MEASURE").fixed(),
        Column::new("WEIGHT").number(),
        Column::new("SCALE").number(),
        Column::new("RAW").number(),
        Column::new("FLOOR").number(),
    ])
    .keep_all_columns()
    .trailing_notice(format!(
        "season {} · calibrated on {} to {} · {} tasks · n_ref {} · {} draws · {} seasons on file",
        number(&season["season"]),
        text(&season["since"]),
        text(&season["until"]),
        number(&season["tasks"]),
        number(&season["n_ref"]),
        number(&season["draws"]),
        number(&value["seasons_on_file"]),
    ));
    let precise = |value: &Value| match value.as_f64() {
        Some(value) => format!("{value:.4}"),
        None => "-".to_string(),
    };
    for scale in season["scales"].as_array().into_iter().flatten() {
        table.add_row(vec![
            text(&scale["key"]),
            number(&scale["weight"]),
            precise(&scale["scale"]),
            precise(&scale["raw"]),
            precise(&scale["floor"]),
        ]);
    }
    vec![Block::table(table)]
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
    // Only implementer counts read the execution scope.
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
