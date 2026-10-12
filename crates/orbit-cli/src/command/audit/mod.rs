mod command;
mod export;
mod list;
mod prune;
mod show;
mod stats;
mod support;

pub use command::{AuditCommand, AuditSubcommand};

/// Whether `audit export --format json` also requests JSON for its
/// confirmation. A global output format, when supplied, still takes priority.
pub(crate) fn export_format_requests_json(matches: &clap::ArgMatches) -> bool {
    let Some(export_matches) = matches
        .subcommand_matches("audit")
        .and_then(|audit| audit.subcommand_matches("export"))
    else {
        return false;
    };
    export_matches.value_source("format") == Some(clap::parser::ValueSource::CommandLine)
        && export_matches
            .try_get_one::<export::ExportFormat>("format")
            .ok()
            .flatten()
            .is_some_and(|format| matches!(format, export::ExportFormat::Json))
}
