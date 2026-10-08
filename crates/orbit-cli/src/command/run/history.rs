use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::application::job::{JobRunListParams, job_run_task_ids};
use orbit_types::workflow::JobRunState;
use serde_json::json;

use crate::command::{Block, CommandOut, Execute, Payload};
use crate::output::color::Domain;

use super::format::{format_duration, format_history_role, format_timestamp, format_waiting_line};
use super::job::cli_job_run_to_json;
use super::steps::RunRead;

const DEFAULT_HISTORY_LIMIT: usize = 50;

// Use the wire-state parser rather than Clap's enum help, which includes
// implementation notes from the persisted domain type.
fn parse_run_state(raw: &str) -> Result<JobRunState, String> {
    raw.parse()
}

#[derive(Args)]
#[command(
    after_help = "JSON shape: {\"runs\":[<job-run>]}\nROLE says how a run was submitted: top-level directly, child by a parent run.\nRun ids minted before role markers existed read as unmarked.\nExamples:\n  orbit run history\n  orbit run history -j task_local_pipeline --limit 20\n  orbit run history --state failed,held --since 24h\n  orbit run history --json\n  orbit run history --limit 200 --no-reconcile --json"
)]
pub struct RunHistoryArgs {
    /// Filter to one job ID
    #[arg(short = 'j', long = "job")]
    pub job_id: Option<String>,

    /// Filter to a task in the submitted task_ids array
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
    /// finalizes an orphaned pending or running run as interrupted and
    /// releases its task reservations
    #[arg(long)]
    pub no_reconcile: bool,
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
        )
    }
}

pub(crate) fn run_history_payload(
    runtime: &OrbitRuntime,
    params: JobRunListParams,
    read: RunRead,
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

    let values = runs
        .iter()
        .zip(states.iter())
        .map(|(run, state)| cli_job_run_to_json(run, *state))
        .collect::<Vec<_>>();
    let doc = json!({ "runs": values });

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
    let mut table = Table::new(columns).empty_message("no runs recorded");
    for (run, state) in runs.iter().zip(states.iter()) {
        use comfy_table::Cell;
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
