use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::application::job::{
    JobRunListParams, fold_run_incidents, job_run_task_ids, run_error_step,
};
use orbit_types::workflow::{JobRun, JobRunState, PipelineState};
use serde_json::json;

use crate::command::{Block, CommandOut, Execute, Payload};
use crate::output::color::Domain;

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
    after_help = "JSON shape: {\"runs\":[<job-run>]}\nROLE says how a run was submitted: top-level directly, child by a parent run.\nRun ids minted before role markers existed read as unmarked.\nA gate or auto run that failed only because a child failed records the leaf it echoes in `.root_cause` ({leaf_run_id, task_id, step, code}); it is null for a run that failed on its own. --incidents folds every listed run that echoes the same leaf into one row, the leaf's own when it is listed: CASCADED counts the folded runs and `.cascaded_run_ids` names them.\nExamples:\n  orbit run history\n  orbit run history -j task_local_pipeline --limit 20\n  orbit run history --state failed,held --since 24h\n  orbit run history --state failed --incidents\n  orbit run history --json\n  orbit run history --limit 200 --no-reconcile --json"
)]
pub struct RunHistoryArgs {
    /// Filter to one job ID
    #[arg(short = 'j', long = "job")]
    pub job_id: Option<String>,

    /// Filter to runs whose input.task_ids array contains the task or whose top-level input.task_id matches it
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

    /// Report stored run records as-is: skip stale-run reconciliation, which
    /// finalizes an orphaned pending or running run as interrupted, releases
    /// its task reservations, and settles with their owner the pull claims an
    /// ended drain or leaf left
    #[arg(long)]
    pub no_reconcile: bool,

    /// Show one row per incident: fold runs that failed only because a child
    /// failed into the run that failed on its own
    #[arg(long)]
    pub incidents: bool,
}

impl Execute for RunHistoryArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        run_history_payload(
            runtime,
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
        )
    }
}

pub(crate) fn run_history_payload(
    runtime: &OrbitRuntime,
    params: JobRunListParams,
    read: RunRead,
    incidents: bool,
) -> CommandOut {
    let include_job_id = params.job_id.is_none();
    let state_filtered = params.state.is_some() || !params.states.is_empty();
    let runs = read.list(runtime, params)?;
    let run_ids = runs
        .iter()
        .map(|run| run.run_id.clone())
        .collect::<Vec<_>>();
    let states = runtime.read_run_states(&run_ids)?;
    let states = runs
        .iter()
        .map(|run| states.get(&run.run_id).and_then(Option::as_ref))
        .collect::<Vec<_>>();

    // [ORB-15202] Each incident keeps one row: its leaf's when listed.
    let rows = if incidents {
        fold_run_incidents(&runs, &states)
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
            value
        })
        .collect::<Vec<_>>();
    let doc = json!({ "runs": values });

    let errors = rows
        .iter()
        .map(|(index, _)| history_error(&runs[*index], states[*index]))
        .collect::<Vec<_>>();
    let show_errors = errors.iter().any(Option::is_some);

    use crate::output::table::{Column, Table};
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
        Column::new("TASK").fixed().filtered(true),
        Column::new("ATTEMPT").number(),
        Column::new("STATE").fixed().filtered(state_filtered),
        Column::new("STARTED_AT").fixed(),
        Column::new("FINISHED_AT").fixed(),
        Column::new("DURATION").fixed().filtered(true),
    ]);
    if incidents {
        columns.push(Column::new("CASCADED").number());
    }
    // Appended last, so the columns before it keep their positions for `cut`.
    // Kept even when every row repeats one error: a shared root cause is the finding.
    if show_errors {
        columns.push(Column::new("ERROR").filtered(true));
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
            crate::output::color::cell(&run.state.to_string(), Domain::JobState),
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
    Ok(Payload::blocks(doc, blocks).into())
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
