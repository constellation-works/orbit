//! `orbit task flow` — inflow-versus-outflow rates for a task population.
//!
//! A non-blocking review model files findings instead of blocking merges, so
//! the question that decides whether it is working cannot be answered from a
//! snapshot: are findings closing as fast as they are filed? An open count
//! alone cannot tell the two apart — a steady forty open tasks is healthy when
//! forty close a week and terminal when none do. This bucketizes both rates
//! over the same windows so the trend is visible.
//!
//! Terminal transitions come from task status history, so a task that is
//! rejected and later reopened remains closed for the intervening windows and
//! still contributes its dropped event. Legacy terminal tasks without status
//! history retain the previous `updated_at` approximation. A terminal status
//! reclassified into another terminal status without an intervening reopen
//! (for example `Done` later `Archived`) contributes only its first outflow
//! event — the later one is a relabeling, not a second departure.

use chrono::{DateTime, Duration, Utc};
use clap::{ArgAction, Args};
use orbit_core::{OrbitError, OrbitRuntime, Task, TaskStatus, TaskType};
use orbit_types::task::TaskHistoryEntry;
use serde_json::json;

use crate::command::{Block, CommandOut, Execute, Payload};
use crate::output::table::{Column, Table};
use crate::parse::parse_duration_seconds;

/// Default bucket width. A week matches how the review sweeps are scheduled
/// and how a human reads backlog movement.
const DEFAULT_WINDOW: &str = "7d";
/// Default number of buckets. Six weeks is long enough for a trend to separate
/// itself from one noisy week.
const DEFAULT_BUCKETS: usize = 6;

#[derive(Args)]
#[command(
    about = "Show backlog inflow-vs-outflow rates over time (is it draining?)",
    after_help = "Examples:\n  orbit task flow\n  orbit task flow --tag code-review\n  orbit task flow --tag security-review --window 14d --buckets 4\n  orbit task flow --type bug --json\n\n\
                  FILED counts tasks created in the window. REOPENED counts terminal tasks that\n\
                  re-entered the backlog. CLOSED counts tasks that reached\n\
                  `done`; DROPPED counts `rejected` and `archived`, which clear the backlog\n\
                  without fixing anything — a drain driven by DROPPED is not the same result\n\
                  as one driven by CLOSED. NET is FILED plus REOPENED minus both outflows.\n\
                  OPEN AT END is how much of the population was still open when that window\n\
                  closed.\n\n\
                  The verdict compares total inflow against total outflow across every\n\
                  window: draining, flat, or growing.\n\n\
                  Status transitions come from task history, so reopened tasks retain their prior\n\
                  CLOSED or DROPPED events while their re-entry is counted as fresh inflow."
)]
pub struct TaskFlowArgs {
    /// Filter by tag. Repeat for AND semantics, matching `orbit task list`.
    #[arg(long = "tag", action = ArgAction::Append, value_delimiter = ',')]
    pub tags: Vec<String>,
    /// Filter by task type (feature, bug, refactor, chore)
    #[arg(long = "type", value_enum)]
    pub task_type: Option<TaskType>,
    /// Width of each bucket as a duration (s/m/h/d/w). Default 7d.
    #[arg(long, default_value = DEFAULT_WINDOW)]
    pub window: String,
    /// Number of buckets to report, most recent last. Default 6.
    #[arg(long, default_value_t = DEFAULT_BUCKETS, value_parser = parse_buckets)]
    pub buckets: usize,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// The task creation time and status changes the report reads. Reducing to
/// them keeps the arithmetic testable without constructing whole tasks.
#[derive(Clone)]
pub(crate) struct FlowPoint {
    pub created_at: DateTime<Utc>,
    pub status_changes: Vec<StatusChange>,
}

/// A transition into a task status at a known instant.
#[derive(Clone, Copy)]
pub(crate) struct StatusChange {
    pub at: DateTime<Utc>,
    pub status: TaskStatus,
}

/// A task leaves the live backlog on `done`, `rejected`, or `archived`. The
/// first is delivery; the other two clear the queue without it, which is why
/// they are counted apart rather than summed into one "closed" number.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TerminalKind {
    Closed,
    Dropped,
}

impl TerminalKind {
    fn of(status: TaskStatus) -> Option<Self> {
        match status {
            TaskStatus::Done => Some(Self::Closed),
            TaskStatus::Rejected | TaskStatus::Archived => Some(Self::Dropped),
            _ => None,
        }
    }
}

impl FlowPoint {
    pub(crate) fn from_task(task: &Task, history: &[TaskHistoryEntry]) -> Self {
        let mut status_changes: Vec<StatusChange> = history
            .iter()
            .filter_map(|entry| {
                entry.to_status.map(|status| StatusChange {
                    at: entry.at,
                    status,
                })
            })
            .collect();
        status_changes.sort_by_key(|change| change.at);

        // Older task records may predate status history. Preserve their
        // terminal outflow approximation rather than silently removing them
        // from the report.
        if status_changes.is_empty() && TerminalKind::of(task.status).is_some() {
            status_changes.push(StatusChange {
                at: task.updated_at,
                status: task.status,
            });
        }

        Self {
            created_at: task.created_at,
            status_changes,
        }
    }

    /// Whether the task was still open at `instant`: created by then and not
    /// in a terminal status at that time. A later reopen restores it to the
    /// historical backlog population.
    fn open_at(&self, instant: DateTime<Utc>) -> bool {
        self.created_at <= instant
            && self
                .status_changes
                .iter()
                .rev()
                .find(|change| change.at <= instant)
                .is_none_or(|change| TerminalKind::of(change.status).is_none())
    }

    /// Nonterminal entries immediately following a terminal status. These
    /// restore a task to the live backlog and therefore offset the earlier
    /// terminal outflow in rate arithmetic.
    fn reopen_times(&self) -> impl Iterator<Item = DateTime<Utc>> + '_ {
        self.status_changes.windows(2).filter_map(|changes| {
            (TerminalKind::of(changes[0].status).is_some()
                && TerminalKind::of(changes[1].status).is_none())
            .then_some(changes[1].at)
        })
    }

    /// Terminal transitions that are genuine departures from the backlog.
    ///
    /// A terminal status immediately following another terminal status (for
    /// example `Done` reclassified as `Archived`) never passed back through
    /// the live backlog, so it is not a second outflow — only the first
    /// terminal transition in a contiguous run counts.
    fn outflow_events(&self) -> impl Iterator<Item = (DateTime<Utc>, TerminalKind)> + '_ {
        self.status_changes
            .iter()
            .enumerate()
            .filter_map(|(index, change)| {
                let kind = TerminalKind::of(change.status)?;
                let follows_terminal =
                    index > 0 && TerminalKind::of(self.status_changes[index - 1].status).is_some();
                (!follows_terminal).then_some((change.at, kind))
            })
    }
}

/// One bucket's inflow and outflow, plus the population still open at its end.
pub(crate) struct Bucket {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub filed: usize,
    pub reopened: usize,
    pub closed: usize,
    pub dropped: usize,
    pub open_at_end: usize,
}

impl Bucket {
    pub(crate) fn net(&self) -> i64 {
        self.filed as i64 + self.reopened as i64 - self.closed as i64 - self.dropped as i64
    }
}

pub(crate) struct FlowReport {
    pub buckets: Vec<Bucket>,
    pub filed: usize,
    pub reopened: usize,
    pub closed: usize,
    pub dropped: usize,
    pub open_now: usize,
}

impl FlowReport {
    pub(crate) fn net(&self) -> i64 {
        self.filed as i64 + self.reopened as i64 - self.closed as i64 - self.dropped as i64
    }

    /// Inflow against outflow across the whole reported span, worded as the
    /// answer to the question the command exists to ask.
    ///
    /// An empty population reports that it has no data rather than "flat":
    /// zero equals zero is arithmetically a match and substantively nothing,
    /// and a caller who filtered down to no tasks must not read it as health.
    pub(crate) fn verdict(&self) -> &'static str {
        if self.filed == 0
            && self.reopened == 0
            && self.closed == 0
            && self.dropped == 0
            && self.open_now == 0
        {
            return "no data — no tasks matched in the reported span";
        }
        match self.net() {
            net if net > 0 => "growing — filed faster than cleared",
            0 => "flat — inflow and outflow match",
            _ => "draining — cleared faster than filed",
        }
    }
}

/// Bucket the population into `count` windows of `width`, oldest first, ending
/// at `now`. `now` is a parameter rather than read inside so the arithmetic is
/// deterministic under test.
pub(crate) fn compute_flow(
    points: &[FlowPoint],
    now: DateTime<Utc>,
    width: Duration,
    count: usize,
) -> FlowReport {
    let mut buckets: Vec<Bucket> = (0..count)
        .rev()
        .map(|index| {
            let end = now - width * (index as i32);
            Bucket {
                start: end - width,
                end,
                filed: 0,
                reopened: 0,
                closed: 0,
                dropped: 0,
                open_at_end: 0,
            }
        })
        .collect();

    for bucket in &mut buckets {
        for point in points {
            if point.created_at >= bucket.start && point.created_at < bucket.end {
                bucket.filed += 1;
            }
            bucket.reopened += point
                .reopen_times()
                .filter(|at| *at >= bucket.start && *at < bucket.end)
                .count();
            for (at, kind) in point.outflow_events() {
                if at >= bucket.start && at < bucket.end {
                    match kind {
                        TerminalKind::Closed => bucket.closed += 1,
                        TerminalKind::Dropped => bucket.dropped += 1,
                    }
                }
            }
            if point.open_at(bucket.end) {
                bucket.open_at_end += 1;
            }
        }
    }

    FlowReport {
        filed: buckets.iter().map(|bucket| bucket.filed).sum(),
        reopened: buckets.iter().map(|bucket| bucket.reopened).sum(),
        closed: buckets.iter().map(|bucket| bucket.closed).sum(),
        dropped: buckets.iter().map(|bucket| bucket.dropped).sum(),
        open_now: points.iter().filter(|point| point.open_at(now)).count(),
        buckets,
    }
}

impl Execute for TaskFlowArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let width = bucket_width(&self.window)?;
        let points: Vec<FlowPoint> = runtime
            .list_tasks_by_tags(&self.tags)?
            .iter()
            .filter(|task| self.task_type.is_none_or(|kind| task.task_type == kind))
            .map(|task| {
                runtime
                    .get_task_history(&task.id)
                    .map(|history| FlowPoint::from_task(task, &history))
            })
            .collect::<Result<_, _>>()?;

        let report = compute_flow(&points, Utc::now(), width, self.buckets);

        let mut table = Table::new(vec![
            Column::new("WINDOW").fixed(),
            Column::new("FILED").number(),
            Column::new("REOPENED").number(),
            Column::new("CLOSED").number(),
            Column::new("DROPPED").number(),
            Column::new("NET").number(),
            Column::new("OPEN AT END").number(),
        ])
        .keep_all_columns()
        .empty_message("no windows to report");

        for bucket in &report.buckets {
            table.add_row(vec![
                bucket.start.format("%Y-%m-%d").to_string(),
                bucket.filed.to_string(),
                bucket.reopened.to_string(),
                bucket.closed.to_string(),
                bucket.dropped.to_string(),
                format_net(bucket.net()),
                bucket.open_at_end.to_string(),
            ]);
        }

        let doc = json!({
            "window": self.window,
            "buckets": report
                .buckets
                .iter()
                .map(|bucket| json!({
                    "start": bucket.start.to_rfc3339(),
                    "end": bucket.end.to_rfc3339(),
                    "filed": bucket.filed,
                    "reopened": bucket.reopened,
                    "closed": bucket.closed,
                    "dropped": bucket.dropped,
                    "net": bucket.net(),
                    "open_at_end": bucket.open_at_end,
                }))
                .collect::<Vec<_>>(),
            "totals": {
                "filed": report.filed,
                "reopened": report.reopened,
                "closed": report.closed,
                "dropped": report.dropped,
                "net": report.net(),
                "open_now": report.open_now,
            },
            "verdict": report.verdict(),
        });

        let summary = format!(
            "{} filed, {} reopened, {} closed, {} dropped over {} × {} — net {}, {} open now: {}",
            report.filed,
            report.reopened,
            report.closed,
            report.dropped,
            self.buckets,
            self.window,
            format_net(report.net()),
            report.open_now,
            report.verdict(),
        );
        Ok(Payload::blocks(doc, vec![Block::table(table), Block::text(summary)]).into())
    }
}

/// Signed rendering, so a negative net — the healthy direction — is
/// unmistakable next to an unsigned count.
pub(crate) fn format_net(net: i64) -> String {
    if net > 0 {
        format!("+{net}")
    } else {
        net.to_string()
    }
}

/// Bucket width as a `chrono::Duration`, rejecting a zero width that would
/// produce empty windows.
fn bucket_width(raw: &str) -> Result<Duration, OrbitError> {
    let seconds = parse_duration_seconds(raw)?;
    if seconds == 0 {
        return Err(OrbitError::InvalidInput(
            "window must be longer than zero".to_string(),
        ));
    }
    let seconds = i64::try_from(seconds)
        .map_err(|_| OrbitError::InvalidInput(format!("window '{raw}' is too large")))?;
    Duration::try_seconds(seconds)
        .ok_or_else(|| OrbitError::InvalidInput(format!("window '{raw}' is too large")))
}

/// Reject a zero bucket count, which would report nothing at all.
fn parse_buckets(raw: &str) -> Result<usize, String> {
    let value: usize = raw.parse().map_err(|_| {
        format!("`{raw}` is not a valid bucket count (expected a positive integer)")
    })?;
    if value == 0 {
        return Err("buckets must be at least 1".to_string());
    }
    Ok(value)
}
