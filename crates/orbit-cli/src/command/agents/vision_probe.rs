use clap::Args;
use serde_json::Value;

use super::support::{number, text};
use crate::output::common::payload::Block;
use crate::output::common::table::{Column, Table};

pub(super) const VISION_PROBE_LONG_ABOUT: &str = "Check that each crew's model sees images.

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

/// One row per probed crew: executor, mode, result and any visual hold.
pub(super) fn vision_probe_blocks(value: &Value) -> Vec<Block> {
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
