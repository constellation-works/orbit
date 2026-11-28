use clap::Args;

#[derive(Args)]
#[command(
    after_help = "Without --confirm this only reports. Prepared, committed and aborted rows, \
                  and each task's newest applied row, are never deleted.\n\n\
                  Examples:\n  \
                  orbit gc journal\n  \
                  orbit gc journal --confirm --json",
    after_long_help = "Without --confirm this only reports. Only applied rows of this workspace, \
                       and of workspace ids that are no longer registered, are selected; another \
                       registered workspace keeps its own window. An applied row is a commit \
                       decision already published to its task, so recovery never reads it again. \
                       Prepared and committed rows, which recovery replays, aborted rows, and \
                       each task's newest applied row are never deleted.\n\n\
                       How rows are selected and deleted, and how freed pages return to the \
                       filesystem: installed orbit-setup skill \
                       `references/maintenance-storage.md`.\n\n\
                       Examples:\n  \
                       orbit gc journal\n  \
                       orbit gc journal --confirm --json"
)]
pub struct JournalGcArgs {
    /// Delete what the plan lists; without this flag the command only reports
    #[arg(long, visible_alias = "apply")]
    pub confirm: bool,

    /// Retention window in days (default 14)
    #[arg(long, value_name = "DAYS")]
    pub older_than_days: Option<u32>,
}
