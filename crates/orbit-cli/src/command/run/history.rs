use orbit_core::app::run::FailureRow;

use clap::Args;
use orbit_core::Core;
use orbit_core::app::run::{Command, exec};
use orbit_core::application::job::{
    JobRunListParams, fold_run_incidents, job_run_task_ids, run_error_step, run_upgrade_paused,
};
use orbit_types::workflow::{JobRun, JobRunState, PipelineState};
use serde_json::json;

use crate::command::{Block, CommandOut, ExecuteCore, Payload};
use crate::output::common::color::Domain;

use super::format::{format_duration, format_history_role, format_timestamp, format_waiting_line};
use super::job::cli_job_run_to_json;
use super::steps::RunRead;

const DEFAULT_HISTORY_LIMIT: usize = 50;
/// The ERROR column's cap, counting the ellipsis, so one row stays scannable.
const ERROR_MAX_CHARS: usize = 60;

// Use the wire-state parser rather than Clap's enum help, which includes
// implementation notes from the persisted domain type.
fn parse_run_state(raw: &str) -> Result<JobRunState, String> {
    raw.parse()
}

#[derive(Args)]
#[command(
    after_help = "ROLE says how a run was submitted: top-level directly, child by a parent run.\nJSON shapes, ROLE markers and how --incidents and --failures fold runs: installed orbit-orchestrate skill `references/run-debugging-history.md`.\nExamples:\n  orbit run history\n  orbit run history -j task_local_pipeline --limit 20\n  orbit run history --state failed,held --since 24h\n  orbit run history --state failed --incidents\n  orbit run history --state failed --failures\n  orbit run history --json\n  orbit run history --limit 200 --no-reconcile --json"
)]
pub struct RunHistoryArgs {
    /// Filter to one job ID
    #[arg(short = 'j', long = "job")]
    pub job_id: Option<String>,

    /// Filter to runs whose input names this task
    #[arg(long = "task")]
    pub task_id: Option<String>,

    /// Filter to any of these comma-separated run states
    #[arg(long, value_delimiter = ',', value_parser = parse_run_state)]
    pub state: Vec<JobRunState>,

    /// Only runs created at or after this RFC 3339 time or relative duration (24h, 7d)
    #[arg(long)]
    pub since: Option<String>,

    /// Maximum number of runs to show
    #[arg(long, default_value_t = DEFAULT_HISTORY_LIMIT, value_parser = crate::parse::positive_limit)]
    pub limit: usize,

    /// Report stored run records as-is, skipping stale-run reconciliation
    #[arg(long)]
    pub no_reconcile: bool,

    /// One row per incident, folding runs that failed only because a child did
    #[arg(long, conflicts_with = "failures")]
    pub incidents: bool,

    /// One row per task and root cause, with that run's step, crew and cause
    #[arg(long, conflicts_with = "incidents")]
    pub failures: bool,
}

impl ExecuteCore for RunHistoryArgs {
    fn execute(self, core: &Core) -> CommandOut {
        run_history_payload(
            core,
            JobRunListParams {
                job_id: self.job_id,
                task_id: self.task_id,
                states: self.state,
                since: self
                    .since
                    .as_deref()
                    .map(crate::parse::parse_since)
                    .transpose()?,
                limit: Some(self.limit),
                ..Default::default()
            },
            RunRead::from_no_reconcile(self.no_reconcile),
            self.incidents,
            self.failures,
        )
    }
}

pub(crate) fn run_history_payload(
    core: &Core,
    params: JobRunListParams,
    read: RunRead,
    incidents: bool,
    failures: bool,
) -> CommandOut {
    let include_job_id = params.job_id.is_none();
    let task_filtered = params.task_id.is_some();
    let state_filtered = params.state.is_some() || !params.states.is_empty();
    let history = exec(core, Command::history(params, read, failures))?.value;
    if let Some((rows, paused)) = history.failures {
        return failure_history_payload(&rows, paused);
    }
    let runs = history.runs;
    let states = history.states.for_runs(&runs);

    // [ORB-15202] Each incident keeps one row: its leaf's when listed. An
    // upgrade's designed pause is no incident; it is only counted.
    let mut upgrade_paused = 0;
    let rows = if incidents {
        let (paused, incidents): (Vec<_>, Vec<_>) = fold_run_incidents(&runs, &states)
            .into_iter()
            .partition(|incident| incident.upgrade_paused);
        upgrade_paused = paused.len();
        incidents
            .into_iter()
            .map(|incident| (incident.index, Some(incident.cascaded_run_ids)))
            .collect::<Vec<_>>()
    } else {
        (0..runs.len()).map(|index| (index, None)).collect()
    };
    let values = rows
        .iter()
        .map(|(index, cascaded)| {
            let mut value = cli_job_run_to_json(&runs[*index], states[*index]);
            if let Some(cascaded) = cascaded {
                value["cascaded_run_ids"] = json!(cascaded);
            }
            if run_upgrade_paused(&runs[*index], states[*index]) {
                value["upgrade_paused"] = json!(true);
            }
            value
        })
        .collect::<Vec<_>>();
    let mut doc = json!({ "runs": values });
    if upgrade_paused > 0 {
        doc["upgrade_paused"] = json!(upgrade_paused);
    }

    let errors = rows
        .iter()
        .map(|(index, _)| history_error(&runs[*index], states[*index]))
        .collect::<Vec<_>>();
    let show_errors = errors.iter().any(Option::is_some);

    use crate::output::common::table::{Column, Table};
    let mut columns = vec![
        Column::new("RUN_ID").fixed(),
        // Sibling top-level runs and one run's children share a minute stem, so
        // the id's own role marker is what keeps a listing from reading as a
        // single run tree when it is several [ORB-12111].
        Column::new("ROLE").fixed(),
    ];
    if include_job_id {
        columns.push(Column::new("JOB_ID").fixed());
    }
    // `orbit run show <run_id>` prints a run's untruncated error message.
    columns.extend([
        // Identity and timing stay visible even when a task filter makes them uniform.
        Column::new("TASK")
            .fixed()
            .keep_when_uniform(true)
            .filtered(task_filtered),
        Column::new("ATTEMPT").number(),
        Column::new("STATE").fixed().filtered(state_filtered),
        Column::new("STARTED_AT").fixed(),
        Column::new("FINISHED_AT").fixed(),
        Column::new("DURATION").fixed().keep_when_uniform(true),
    ]);
    if incidents {
        columns.push(Column::new("CASCADED").number());
    }
    // Appended last, so the columns before it keep their positions for `cut`.
    // Kept even when every row repeats one error: a shared root cause is the finding.
    if show_errors {
        columns.push(Column::new("ERROR").keep_when_uniform(true));
    }
    let mut table = Table::new(columns).empty_message("no runs recorded");
    for ((index, cascaded), error) in rows.iter().zip(&errors) {
        use comfy_table::Cell;
        let (run, state) = (&runs[*index], &states[*index]);
        let mut row = vec![
            Cell::new(&run.run_id),
            Cell::new(format_history_role(
                &run.run_id,
                state.and_then(|state| state.trigger.as_ref()),
            )),
        ];
        if include_job_id {
            row.push(Cell::new(&run.job_id));
        }
        row.extend([
            Cell::new(job_run_task_ids(run).join(", ")),
            Cell::new(run.attempt.to_string()),
            crate::output::common::color::cell(&run.state.to_string(), Domain::JobState),
            Cell::new(format_timestamp(run.started_at)),
            Cell::new(format_timestamp(run.finished_at)),
            Cell::new(
                run.duration_ms
                    .map(|ms| format_duration(Some(ms)))
                    .unwrap_or_default(),
            ),
        ]);
        if let Some(cascaded) = cascaded {
            row.push(Cell::new(cascaded.len().to_string()));
        }
        if show_errors {
            row.push(Cell::new(error.as_deref().unwrap_or_default()));
        }
        table.add_row(row);
    }
    let mut blocks = vec![Block::table(table)];
    let waiting = runs
        .iter()
        .zip(states.iter())
        .filter_map(|(run, state)| format_waiting_line(run.state, *state))
        .collect::<Vec<_>>();
    if !waiting.is_empty() {
        blocks.push(Block::text(waiting.join("\n")));
    }
    if upgrade_paused > 0 {
        blocks.push(Block::text(upgrade_paused_note(upgrade_paused)));
    }
    Ok(Payload::blocks(doc, blocks).with_records(values).into())
}

/// The line that discloses upgrade pauses left out of a failure listing.
fn upgrade_paused_note(count: usize) -> String {
    format!("Upgrade-paused, not failures: {count}")
}

fn failure_history_payload(rows: &[FailureRow], paused: usize) -> CommandOut {
    let values = rows
        .iter()
        .map(|row| {
            json!({
                "task_id": row.task_id,
                "run_id": row.run_id,
                "step": row.step,
                "crew": row.crew,
                "cause": row.cause,
                "show": format!("orbit run show {}", row.run_id),
            })
        })
        .collect::<Vec<_>>();
    let mut doc = json!({ "failures": values });

    use crate::output::common::table::{Column, Table};
    let columns = vec![
        Column::new("TASK").fixed(),
        Column::new("RUN_ID").fixed(),
        Column::new("STEP").fixed(),
        Column::new("CREW").fixed(),
        Column::new("CAUSE"),
        Column::new("SHOW").fixed(),
    ];
    let mut table = Table::new(columns)
        .empty_message("no failures recorded")
        .keep_all_columns();
    for row in rows {
        use comfy_table::Cell;
        table.add_row(vec![
            Cell::new(row.task_id.as_deref().unwrap_or("-")),
            Cell::new(&row.run_id),
            Cell::new(row.step.as_deref().unwrap_or("-")),
            Cell::new(row.crew.as_deref().unwrap_or("-")),
            Cell::new(row.cause.as_deref().unwrap_or("-")),
            Cell::new(format!("orbit run show {}", row.run_id)),
        ]);
    }
    if paused == 0 {
        return Ok(Payload::enveloped_list(doc, values, table).into());
    }
    doc["upgrade_paused"] = json!(paused);
    let blocks = vec![
        Block::table(table),
        Block::text(upgrade_paused_note(paused)),
    ];
    Ok(Payload::blocks(doc, blocks).with_records(values).into())
}

/// The ERROR cell for one run: the root cause it echoes, else its own failing
/// step's error. Only a run that ended failed, held, interrupted or timed out
/// has one, so every other row stays blank.
fn history_error(run: &JobRun, state: Option<&PipelineState>) -> Option<String> {
    if !matches!(
        run.state,
        JobRunState::Failed | JobRunState::Held | JobRunState::Interrupted | JobRunState::Timeout
    ) {
        return None;
    }
    let text = state
        .and_then(|state| state.root_cause.as_ref())
        .map(|cause| {
            let mut text = format!("root cause {}", cause.leaf_run_id);
            if let Some(step) = cause.step.as_deref() {
                text.push_str(&format!(" at {step}"));
            }
            if let Some(code) = cause.code.as_deref() {
                text.push_str(&format!(" ({code})"));
            }
            text
        })
        .or_else(|| {
            run_error_step(run).and_then(|step| {
                match (step.error_code.as_deref(), step.error_message.as_deref()) {
                    (Some(code), Some(message)) => Some(format!("{code}: {message}")),
                    (Some(text), None) | (None, Some(text)) => Some(text.to_string()),
                    (None, None) => None,
                }
            })
        })?;
    let text = cap_one_line(&text, ERROR_MAX_CHARS);
    (!text.is_empty()).then_some(text)
}

/// `text` on one line of at most `max_chars` characters; a longer value keeps
/// its start and ends in an ellipsis. Whitespace runs, tabs included, collapse
/// to single spaces so a cell cannot split a tab-separated row.
fn cap_one_line(text: &str, max_chars: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= max_chars {
        return line;
    }
    let kept = line.chars().take(max_chars - 3).collect::<String>();
    format!("{}...", kept.trim_end())
}
