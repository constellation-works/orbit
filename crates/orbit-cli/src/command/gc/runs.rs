use clap::Args;

#[derive(Args)]
#[command(
    after_help = "Without --confirm this only reports. A held run and every non-terminal run \
                  are never touched. The run row, its steps and its summary stay.\n\n\
                  Examples:\n  \
                  orbit gc runs\n  \
                  orbit gc runs --confirm",
    after_long_help = "Without --confirm this only reports. Only success, failed, timeout, \
                       cancelled and interrupted runs of this workspace, and of workspace ids \
                       that are no longer registered, are selected; another registered workspace \
                       keeps its own window. A held run, which review evidence can still resume, \
                       and every non-terminal run are never touched. The run row, its steps and \
                       its summary stay, so `orbit run show` and run history keep working, and \
                       `archived_at` records when the state was dropped.\n\n\
                       Rows kept by count, and the resource-history window: installed \
                       orbit-setup skill `references/maintenance-storage.md`.\n\n\
                       Examples:\n  \
                       orbit gc runs\n  \
                       orbit gc runs --confirm"
)]
pub struct RunGcArgs {
    /// Drop what the plan lists; without this flag the command only reports
    #[arg(long, visible_alias = "apply")]
    pub confirm: bool,

    /// Retention window in days, overriding `retention.runs_days` (default 60, minimum 30)
    #[arg(long, value_name = "DAYS")]
    pub older_than_days: Option<u32>,
}
