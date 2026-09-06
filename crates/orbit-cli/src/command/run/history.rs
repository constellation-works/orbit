use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::application::job::JobRunListParams;
use serde_json::json;

use crate::command::{Block, CommandOut, Execute, Payload};
use crate::output::color::Domain;

use super::format::{format_timestamp, format_waiting_line, summarize_error_message};
use super::job::cli_job_run_to_json;

pub(crate) const DEFAULT_HISTORY_LIMIT: usize = 50;

#[derive(Args)]
#[command(
    after_help = "JSON shape: {\"runs\":[<job-run>]}\nExamples:\n  orbit run history\n  orbit run history -j task_local_pipeline --limit 20\n  orbit run history --json"
)]
pub struct RunHistoryArgs {
    /// Filter to one job ID
    #[arg(short = 'j', long = "job")]
    pub job_id: Option<String>,

    /// Maximum number of runs to show
    #[arg(long, default_value_t = DEFAULT_HISTORY_LIMIT)]
    pub limit: usize,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for RunHistoryArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        run_history_payload(runtime, self.job_id.as_deref(), Some(self.limit))
    }
}

pub(crate) fn run_history_payload(
    runtime: &OrbitRuntime,
    job_id: Option<&str>,
    limit: Option<usize>,
) -> CommandOut {
    let runs = match job_id {
        Some(job_id) => runtime.list_job_runs(JobRunListParams {
            job_id: Some(job_id.to_string()),
            limit,
            ..Default::default()
        })?,
        None => runtime.list_job_runs(JobRunListParams {
            limit,
            ..Default::default()
        })?,
    };

    let states = runs
        .iter()
        .map(|run| runtime.read_run_state(&run.run_id))
        .collect::<Result<Vec<_>, _>>()?;

    let values = runs
        .iter()
        .zip(states.iter())
        .map(|(run, state)| cli_job_run_to_json(run, state.as_ref()))
        .collect::<Vec<_>>();
    let doc = json!({ "runs": values });

    use crate::output::table::{Column, Table};
    let include_job_id = job_id.is_none();
    let mut columns = vec![Column::new("RUN_ID").fixed()];
    if include_job_id {
        columns.push(Column::new("JOB_ID").fixed());
    }
    // `orbit run show <run_id>` prints a run's untruncated error message.
    columns.extend([
        Column::new("ATTEMPT").number(),
        Column::new("STATE").fixed(),
        Column::new("STARTED_AT").fixed(),
        Column::new("FINISHED_AT").fixed(),
        Column::new("ERROR_CODE").fixed(),
        Column::new("ERROR_MESSAGE"),
    ]);
    let mut table = Table::new(columns).empty_message("no runs recorded");
    for run in &runs {
        use comfy_table::Cell;
        let last = run.steps.last();
        let mut row = vec![Cell::new(&run.run_id)];
        if include_job_id {
            row.push(Cell::new(&run.job_id));
        }
        row.extend([
            Cell::new(run.attempt.to_string()),
            crate::output::color::cell(&run.state.to_string(), Domain::JobState),
            Cell::new(format_timestamp(run.started_at)),
            Cell::new(format_timestamp(run.finished_at)),
            Cell::new(last.and_then(|s| s.error_code.as_deref()).unwrap_or("-")),
            Cell::new(summarize_error_message(
                last.and_then(|s| s.error_message.as_deref()),
            )),
        ]);
        table.add_row(row);
    }
    let mut blocks = vec![Block::table(table)];
    let waiting = runs
        .iter()
        .zip(states.iter())
        .filter_map(|(run, state)| format_waiting_line(run.state, state.as_ref()))
        .collect::<Vec<_>>();
    if !waiting.is_empty() {
        blocks.push(Block::text(waiting.join("\n")));
    }
    Ok(Payload::blocks(doc, blocks).into())
}
