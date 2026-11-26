use clap::Args;

#[derive(Args)]
#[command(
    after_help = "Without --confirm this only reports. Only success, failed, timeout, cancelled and \
                  interrupted runs of this workspace, and of workspace ids that are no longer \
                  registered, are selected; another registered workspace keeps its own window. A \
                  held run, which review \
                  evidence can still resume, and every non-terminal run are never touched. The \
                  run row, its steps and its summary stay, so `orbit run show` and run history \
                  keep working, and `archived_at` records when the state was dropped. The \
                  host's per-minute run and host resource history, and the admission \
                  forecasts recorded against it, older than 30 days go in the same pass, \
                  whatever the window, except rows kept at any age by count: the newest \
                  200 deliveries per workspace, \
                  work class and complexity, the newest 10,000 forecasts, and host minutes \
                  from the oldest kept forecast onward.\n\n\
                  Examples:\n  orbit gc runs\n  orbit gc runs --confirm"
)]
pub struct RunGcArgs {
    /// Drop what the plan lists; without this flag the command only reports
    #[arg(long, visible_alias = "apply")]
    pub confirm: bool,

    /// Retention window in days, overriding `retention.runs_days` (default 60, minimum 30)
    #[arg(long, value_name = "DAYS")]
    pub older_than_days: Option<u32>,
}
