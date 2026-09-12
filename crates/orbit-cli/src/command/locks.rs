//! `orbit task locks ...` — task file-lock administration.
//!
//! `list` renders the file-lock projection over active (in-progress/review)
//! tasks and any live reservations. `contention` looks the other way, at the
//! pending backlog, and reports which selectors will serialize it. `reserve`
//! takes a TTL'd claim on a surface before work starts, and `release` clears
//! a stale one by ID.
//!
//! Task lock reservations auto-release in workflow pipelines; `release` is the
//! operator escape hatch for a stale reservation that wedges a run. The
//! underlying `orbit.task.locks` / `orbit.task.locks.reserve` /
//! `orbit.task.locks.release` tools are inactive on the agent MCP surface, so
//! all three reach them through the admin `runtime.run_tool` bypass (mirrors
//! `orbit adr list`, ORB-00289).
//!
//! `reserve` and `release` require the same `operator` or `runner`
//! capability: a caller that can create a reservation — which blocks every
//! other caller from that surface until it expires or is released — must be
//! the same caller trusted to remove one. From a plain shell, claim it with
//! `ORBIT_OPERATOR=1`.

use std::fmt::Write as _;

use clap::{ArgAction, Args, Subcommand};
use orbit_core::{OrbitError, OrbitRuntime, TaskStatus};
use serde_json::{Map, Value, json};

use crate::command::task::output::format_task_locks;
use crate::command::{Block, CommandOut, Execute, Payload, require_confirmation};
use crate::output::table::{Column, Table};
use crate::parse::parse_duration_seconds;

#[derive(Args)]
#[command(about = "Inspect, reserve, and release task file locks")]
pub struct LocksCommand {
    #[command(subcommand)]
    pub command: LocksSubcommand,
}

impl Execute for LocksCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        self.command.execute(runtime)
    }
}

#[derive(Subcommand)]
pub enum LocksSubcommand {
    /// Show files locked by active (in-progress/review) tasks and reservations
    List(LocksListArgs),
    /// Show which files the pending backlog collides on (what caps parallelism)
    Contention(LocksContentionArgs),
    /// Claim a file surface for a bounded window before starting work on it
    Reserve(LocksReserveArgs),
    /// Release a stale task lock reservation (operator/admin escape hatch)
    Release(LocksReleaseArgs),
}

impl Execute for LocksSubcommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match self {
            LocksSubcommand::List(args) => args.execute(runtime),
            LocksSubcommand::Contention(args) => args.execute(runtime),
            LocksSubcommand::Reserve(args) => args.execute(runtime),
            LocksSubcommand::Release(args) => args.execute(runtime),
        }
    }
}

#[derive(Args)]
#[command(about = "Show files locked by active (in-progress/review) tasks and reservations")]
pub struct LocksListArgs {
    /// Output the lock projection as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for LocksListArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let locks = runtime.run_tool("orbit.task.locks", json!({}))?;
        Ok(Payload::detail(locks.clone(), format_task_locks(&locks)).into())
    }
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
                  pruned of paths that no longer exist, unioned across descendants for an\n\
                  epic root. A task declaring nothing locks nothing and is counted apart."
)]
pub struct LocksContentionArgs {
    /// Maximum hotspot rows to show. Default 10.
    #[arg(long, default_value_t = DEFAULT_CONTENTION_LIMIT)]
    pub limit: usize,
    /// Output the contention report as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for LocksContentionArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let report = runtime.task_lock_contention(&[TaskStatus::Backlog])?;

        let mut table = Table::new(vec![
            Column::new("SELECTOR").fixed(),
            Column::new("TASKS").number(),
        ])
        .keep_all_columns()
        .empty_message("no selector is claimed by more than one pending task");
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

        Ok(Payload::blocks(
            doc,
            vec![
                Block::table(table),
                Block::text(contention_summary(&report, self.limit)),
            ],
        )
        .into())
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

/// Matches the reservation TTL the domain applies when none is given. Stated
/// here so `--help` can show it and the parser can round-trip it, not to
/// re-decide it: the domain still owns the 1..=7200s range.
const DEFAULT_RESERVATION_TTL: &str = "30m";

/// Exit code for a denied reservation. A conflict is a real answer, not a
/// command failure, so it renders normally — but an unattended caller has to
/// be able to branch on it without parsing the document. Matches the "valid
/// answer that is not success" code `orbit workspace sync --check` uses.
const RESERVATION_DENIED_EXIT_CODE: i32 = 3;

#[derive(Args)]
#[command(
    about = "Claim a file surface for a bounded window before starting work on it",
    after_help = "Examples:\n  orbit task locks reserve --task <TASK_ID>\n  orbit task locks reserve --file dir:crates/orbit-cli --ttl 2h\n  orbit task locks reserve --file file:README.md --json\n\n\
                  Reserving is how work outside the task pipeline takes the same locks the\n\
                  pipeline takes: an editor session, a manual migration, a long refactor. The\n\
                  claim is atomic and either grants the whole surface or grants nothing and\n\
                  reports who holds the overlap.\n\n\
                  `--task` reserves that task's declared context surface, pruned and expanded\n\
                  exactly as conflict admission expands it. `--file` reserves selectors\n\
                  directly. Exactly one of the two — they are mutually exclusive forms.\n\n\
                  Reservations expire on their own, so the TTL is the safety net for a session\n\
                  that dies holding one. Release early with `orbit task locks release <id>\n\
                  --confirm`; a denied reservation exits 3.\n\n\
                  Reserving requires the `operator` or `runner` capability, same as releasing:\n\
                  a caller that can lock a surface out from under everyone else must be the same\n\
                  caller trusted to clear it. From a plain shell, set `ORBIT_OPERATOR=1`."
)]
pub struct LocksReserveArgs {
    /// Task whose declared context surface to reserve. Repeat or comma-separate
    /// to claim a bundle's combined surface.
    #[arg(
        long = "task",
        value_name = "TASK_ID",
        action = ArgAction::Append,
        value_delimiter = ',',
        conflicts_with = "files",
        required_unless_present = "files"
    )]
    pub task_ids: Vec<String>,
    /// Selector to reserve directly (`file:...`, `dir:...`). Repeat or
    /// comma-separate for several.
    #[arg(
        long = "file",
        value_name = "SELECTOR",
        action = ArgAction::Append,
        value_delimiter = ','
    )]
    pub files: Vec<String>,
    /// How long the claim holds, e.g. `45m`, `2h`. Default 30m, maximum 2h.
    #[arg(long, value_name = "DURATION", default_value = DEFAULT_RESERVATION_TTL)]
    pub ttl: String,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for LocksReserveArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let ttl_seconds = reservation_ttl_seconds(&self.ttl)?;
        let mut input = Map::new();
        if self.files.is_empty() {
            input.insert("task_ids".to_string(), json!(self.task_ids));
        } else {
            input.insert("files".to_string(), json!(self.files));
        }
        input.insert("ttl_seconds".to_string(), json!(ttl_seconds));

        let result = runtime.run_tool("orbit.task.locks.reserve", Value::Object(input))?;
        let granted = result["reserved"].as_bool().unwrap_or(false);
        let exit_code = if granted {
            0
        } else {
            RESERVATION_DENIED_EXIT_CODE
        };
        Ok(Payload::blocks(result.clone(), reservation_blocks(&result))
            .with_exit_code(exit_code)
            .into())
    }
}

/// The TTL as whole seconds. `parse_duration_seconds` accepts the same
/// `s/m/h/d/w` forms as the rest of the CLI; the domain rejects anything
/// outside 1..=7200, so only the conversion to its integer width is checked
/// here — a week-long TTL must reach the domain to be refused by name rather
/// than silently wrapping.
fn reservation_ttl_seconds(raw: &str) -> Result<u64, OrbitError> {
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

#[derive(Args)]
#[command(about = "Release a stale task lock reservation (operator/admin escape hatch)")]
pub struct LocksReleaseArgs {
    /// Reservation ID to release (see `orbit task locks list --json`)
    pub reservation_id: String,
    /// Confirm release of the reservation
    #[arg(long)]
    pub confirm: bool,
}

impl Execute for LocksReleaseArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        require_confirmation(self.confirm, "task lock release")?;
        let mut input = Map::new();
        input.insert(
            "reservation_id".to_string(),
            Value::String(self.reservation_id),
        );
        let value = runtime.run_tool("orbit.task.locks.release", Value::Object(input))?;
        Ok(Payload::document(value).into())
    }
}
