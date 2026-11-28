use clap::Args;

#[derive(Args)]
#[command(
    after_help = "Without --confirm this only reports. A blob still named by a remaining audit \
                  row, run step or pipeline state is kept. Another registered workspace keeps \
                  its own window.\n\n\
                  Examples:\n  \
                  orbit gc audit\n  \
                  orbit gc audit --older-than-days 30 --confirm --json",
    after_long_help = "Without --confirm this only reports. The command audit (`audit_events`) \
                       is host-wide. Run audit rows are this workspace's, plus rows whose \
                       workspace id is no longer registered; another registered workspace keeps \
                       its own window. Blobs are this workspace's. A blob is kept while any \
                       remaining run audit row, run step or pipeline state names it.\n\n\
                       How long a fresh blob is kept, and how deletes run: installed \
                       orbit-setup skill `references/maintenance-storage.md`.\n\n\
                       Examples:\n  \
                       orbit gc audit\n  \
                       orbit gc audit --older-than-days 30 --confirm --json"
)]
pub struct AuditGcArgs {
    /// Delete what the plan lists; without this flag the command only reports
    #[arg(long, visible_alias = "apply")]
    pub confirm: bool,

    /// Retention window in days, overriding `retention.audit_days` (default 60)
    #[arg(long, value_name = "DAYS")]
    pub older_than_days: Option<u32>,
}
