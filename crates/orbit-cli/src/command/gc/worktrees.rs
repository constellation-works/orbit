use clap::Args;

#[derive(Args)]
pub struct WorktreeGcArgs {
    /// Perform removals; without this flag the command only reports
    #[arg(long, visible_alias = "yes", conflicts_with = "dry_run")]
    pub confirm: bool,

    /// Explicitly request the default non-destructive mode
    #[arg(long, conflicts_with = "confirm")]
    pub dry_run: bool,

    /// Restrict collection to one job run
    #[arg(long, value_name = "ID")]
    pub run: Option<String>,

    /// Restrict collection to runs finished at least this many hours ago
    #[arg(long, value_name = "HOURS")]
    pub older_than_hours: Option<u64>,

    /// Walk eligible worktrees to estimate reclaimable bytes. Dry-run skips
    /// this walk by default; `--confirm` always measures before removal.
    #[arg(long)]
    pub estimate_bytes: bool,

    /// Reclaim worktree.reclaim paths and keep the checkout. Reports bytes
    /// per pattern for terminal runs with no live or undecidable worker;
    /// combine with --confirm to delete. --target-only is an alias.
    #[arg(long, alias = "target-only")]
    pub reclaim: bool,
}
