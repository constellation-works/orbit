use clap::Args;

#[derive(Args)]
#[command(
    after_help = "Without --confirm this only reports. The command audit (`audit_events`) is \
                  host-wide. Run audit rows are this workspace's, plus rows whose workspace id is \
                  no longer registered; another registered workspace keeps its own window. Blobs \
                  are this workspace's. A blob is kept while \
                  any remaining run audit row, run step or pipeline state names it, while a \
                  pending-publication marker newer than the cutoff names it, and for 24 hours \
                  after it was last written. Deletes run in batches of 1,000 rows, each its own \
                  write transaction. Freed pages stay in the store file; VACUUM it while Orbit \
                  is idle to return them to the filesystem.\n\nExamples:\n  orbit gc audit\n  \
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
