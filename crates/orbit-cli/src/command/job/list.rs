use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_types::workflow::JobKind;

use crate::command::{CommandOut, Execute, Payload};
use crate::output::color::Domain;

use super::support::{
    format_last_run, job_catalog_filter, job_catalog_target_summary,
    job_catalog_to_json_with_last_run, job_catalog_to_signal_json,
};

#[derive(Args)]
#[command(
    after_help = "Examples:\n  orbit job list\n  orbit job list --all\n  orbit job list --kind subroutine\n  orbit job list --json"
)]
pub struct JobListArgs {
    /// Include disabled jobs
    #[arg(long)]
    pub all: bool,
    /// Filter to one v2 job kind.
    #[arg(long, value_enum)]
    pub kind: Option<JobKind>,
    /// Output signal-tier JSON (job_id, target_id, state only)
    #[arg(long)]
    pub ops: bool,
}

impl Execute for JobListArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let filter = job_catalog_filter(self.all, self.kind);
        let jobs_with_runs = runtime.list_job_catalog_with_last_run(self.all, filter)?;
        // `--ops` picks the narrower record shape; the rows are the same jobs
        // either way, so the renderer still has a table to fall back to.
        let values = if self.ops {
            jobs_with_runs
                .iter()
                .map(|(job, _)| job_catalog_to_signal_json(job))
                .collect::<Vec<_>>()
        } else {
            jobs_with_runs
                .iter()
                .map(|(job, last_run)| job_catalog_to_json_with_last_run(job, last_run.as_ref()))
                .collect::<Vec<_>>()
        };

        use crate::output::table::{Column, Table};
        // `orbit job show <job_id>` prints a job's full definition.
        let mut table = Table::new(vec![
            Column::new("JOB_ID").fixed(),
            Column::new("KIND").fixed(),
            Column::new("TARGET_TYPE").fixed(),
            Column::new("TARGET_ID"),
            Column::new("STATE").fixed(),
            Column::new("LAST_RUN").fixed(),
        ])
        .empty_message("no jobs matching the given filters");
        for (job, last_run) in &jobs_with_runs {
            use comfy_table::Cell;
            let (target_type, target_id) = job_catalog_target_summary(job);
            table.add_row(vec![
                Cell::new(&job.job_id),
                Cell::new(job.kind().to_string()),
                Cell::new(target_type),
                Cell::new(target_id),
                crate::output::color::cell(&job.state().to_string(), Domain::JobState),
                Cell::new(format_last_run(last_run.as_ref())),
            ]);
        }
        Ok(Payload::list(values, table).into())
    }
}
