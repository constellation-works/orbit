use clap::Args;
use clap::builder::TypedValueParser;
use orbit_core::application::gc::DEFAULT_DEPLOYMENT_KEEP;

#[derive(Args)]
#[command(
    after_help = "Without --confirm this only reports. Three classes are counted separately: \
                  top-level entries of ~/.orbit/deployments, copies of replaced executables in \
                  ~/.orbit/bin (names with a .bak, .before-, .pre-, .stale or -stale- marker), \
                  and store copies named ~/.orbit/orbit.db.bak*. Each class keeps its newest \
                  entries by the newest modification time inside them. The installed \
                  executable and every <executable>.previous rollback copy are never removed, \
                  nor is a deployment whose orbit-local-candidate manifest names the bytes of \
                  the installed or rollback executable; those do not count toward --keep. \
                  Links are removed, never followed.\n\nExamples:\n  orbit gc deployments\n  \
                  orbit gc deployments --keep 1 --confirm --json"
)]
pub struct DeploymentGcArgs {
    /// Remove what the report lists; without this flag the command only reports
    #[arg(long, visible_alias = "yes", conflicts_with = "dry_run")]
    pub confirm: bool,

    /// Explicitly request the default non-destructive mode
    #[arg(long, conflicts_with = "confirm")]
    pub dry_run: bool,

    /// Newest entries to keep in each class, besides the rollback-pinned ones
    #[arg(
        long,
        value_name = "N",
        default_value_t = DEFAULT_DEPLOYMENT_KEEP,
        value_parser = clap::value_parser!(u16).range(1..).map(usize::from)
    )]
    pub keep: usize,
}
