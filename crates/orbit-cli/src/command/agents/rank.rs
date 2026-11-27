use clap::{Args, Subcommand};
use orbit_common::OrbitError;
use orbit_core::app::agent::RankQuery;
use serde_json::Value;

use super::support::{number, text};
use crate::output::common::payload::Block;
use crate::output::common::table::{Column, Table};

pub(super) const RANK_LONG_ABOUT: &str =
    "Rank implementer models by quality and reliability, in tie bands.

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
    pub(super) fn query(&self, live: bool) -> Result<RankQuery, OrbitError> {
        RankQuery::parse(
            self.window.as_deref(),
            self.until.as_deref(),
            self.settle_lag.as_deref(),
            self.execution.as_deref(),
            live,
        )
    }
}

/// One row per model in board order, a column per recorded measure's
/// contribution, and notices for the selection, scales and unmeasured
/// measures.
pub(super) fn rank_blocks(value: &Value) -> Vec<Block> {
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
pub(super) fn calibrate_blocks(value: &Value) -> Vec<Block> {
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
