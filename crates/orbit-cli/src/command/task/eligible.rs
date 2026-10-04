use clap::{Args, ValueEnum};
use comfy_table::Cell;
use orbit_core::{DEFAULT_TASK_LIST_LIMIT, OrbitRuntime};
use serde_json::{Value, json};

use crate::command::{Block, CommandOut, Execute, Payload};
use crate::output::color::Domain;
use crate::output::table::{Column, Table};

/// The statuses a candidate can have: work nobody has taken yet.
#[derive(Clone, Copy, ValueEnum)]
pub enum EligibleStatus {
    Backlog,
    Proposed,
}

impl EligibleStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Backlog => "backlog",
            Self::Proposed => "proposed",
        }
    }
}

// Help text lives on the `TaskSubcommand::Eligible` variant.
#[derive(Args)]
#[command(
    after_help = "Examples:\n  orbit task eligible\n  orbit task eligible --status proposed\n  orbit task eligible --path crates/orbit-cli\n  orbit task eligible --explain\n  orbit task eligible --json"
)]
pub struct TaskEligibleArgs {
    /// Candidate statuses (comma-separated). Default: backlog and proposed.
    #[arg(long, value_enum, value_delimiter = ',')]
    pub status: Vec<EligibleStatus>,
    /// Keep only candidates whose `context_files` selectors apply to this
    /// path, matched as `orbit task list --path` matches.
    #[arg(long)]
    pub path: Option<String>,
    /// Maximum eligible tasks to list (default 50). Must be at least 1.
    #[arg(long, default_value_t = DEFAULT_TASK_LIST_LIMIT, value_parser = crate::parse::positive_limit)]
    pub limit: usize,
    /// Also list the held-back candidates, each with the overlapping file and
    /// the in-progress or review task holding it
    #[arg(long)]
    pub explain: bool,
    /// Output the result as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for TaskEligibleArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let mut input = json!({
            "limit": self.limit,
            "explain": self.explain,
        });
        if !self.status.is_empty() {
            input["status"] = self.status.iter().map(|s| s.as_str()).collect();
        }
        if let Some(path) = self.path {
            input["path"] = Value::String(path);
        }
        // One contract: the CLI document is exactly what `orbit.task.eligible`
        // returns over MCP.
        let doc = runtime.run_tool("orbit.task.eligible", input)?;
        let blocks = eligible_blocks(&doc, self.explain);
        Ok(Payload::blocks(doc, blocks).into())
    }
}

fn eligible_blocks(doc: &Value, explain: bool) -> Vec<Block> {
    let tasks = doc["tasks"].as_array().map_or(&[][..], Vec::as_slice);
    let mut table = Table::new(vec![
        Column::new("ID").fixed(),
        Column::new("STATUS").fixed(),
        Column::new("PRIORITY").fixed(),
        Column::new("COMPLEXITY").fixed(),
        Column::new("TITLE"),
    ])
    .empty_message("no eligible tasks");
    for task in tasks {
        table.add_row(vec![
            Cell::new(text(&task["id"])),
            crate::output::color::cell(text(&task["status"]), Domain::TaskStatus),
            crate::output::color::cell(text(&task["priority"]), Domain::Priority),
            Cell::new(task["complexity"].as_str().unwrap_or("-")),
            Cell::new(text(&task["title"])),
        ]);
    }
    if doc["truncated"].as_bool() == Some(true) {
        table = table.trailing_notice(format!(
            "showing {} of {} eligible tasks; use --limit N to see more",
            tasks.len(),
            doc["total"].as_u64().unwrap_or_default()
        ));
    }
    let mut blocks = vec![Block::table(table)];
    if !explain {
        return blocks;
    }

    let conflicting = doc["conflicting"].as_array().map_or(&[][..], Vec::as_slice);
    if conflicting.is_empty() {
        blocks.push(Block::text("No candidate conflicts with in-flight work."));
        return blocks;
    }
    blocks.push(Block::text("Held back by in-progress or review work:"));
    let mut held = Table::new(vec![
        Column::new("ID").fixed(),
        Column::new("STATUS").fixed(),
        Column::new("FILE").path(),
        Column::new("HELD BY").fixed(),
    ])
    .keep_all_columns();
    for task in conflicting {
        for conflict in task["conflicts"].as_array().map_or(&[][..], Vec::as_slice) {
            held.add_row(vec![
                Cell::new(text(&task["id"])),
                crate::output::color::cell(text(&task["status"]), Domain::TaskStatus),
                Cell::new(text(&conflict["requested_file"])),
                Cell::new(text(&conflict["locking_task_id"])),
            ]);
        }
    }
    blocks.push(Block::table(held));
    blocks
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}
