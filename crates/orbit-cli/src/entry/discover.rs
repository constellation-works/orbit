use clap::{Command, CommandFactory, FromArgMatches};

use super::host_ssh::host_ssh_command;
use super::output_args::{install_output_args, plain_header, requested_output};
use crate::command;
use crate::output::sink::FormatArg;
use crate::plugin_cli;
use crate::usage_error;

/// Repoint clap's default "pass it after `--`" tip when the rejected flag is
/// `--crew` on `orbit run job` / `orbit job run` (matched by the `<JOB_ID>`
/// usage, unique to [`crate::command::run::JobRunArgs`]).
///
/// That default tip is actively wrong here: a job run has no positional slot
/// a trailing `--crew` could land in, so following it just produces a second,
/// more confusing error. Crew selection for a job run goes through the
/// existing `--input` contract (`resolve_crew_for_run_input` reads `crew`
/// from the run input), so the repair only rewrites the tip text — it does
/// not change what argv the parser accepts or make `--crew` appear to work.
fn repair_crew_flag_suggestion(mut err: clap::error::Error) -> clap::error::Error {
    use clap::error::{ContextKind, ContextValue};

    let is_crew_flag = matches!(
        err.get(ContextKind::InvalidArg),
        Some(ContextValue::String(arg)) if arg == "--crew"
    );
    let is_job_run = matches!(
        err.get(ContextKind::Usage),
        Some(ContextValue::StyledStr(usage)) if usage.to_string().contains("<JOB_ID>")
    );
    if is_crew_flag && is_job_run {
        err.insert(
            ContextKind::Suggested,
            ContextValue::StyledStrs(vec![clap::builder::StyledStr::from(
                "crew selection is `--input crew=<name>`, e.g. `--input crew=luna` (see `orbit run job --help`)",
            )]),
        );
    }
    err
}

/// Where an explicitly chosen Orbit root came from, if it was chosen at all.
pub(super) fn explicit_root_source(
    root_override: Option<&std::path::Path>,
) -> Option<&'static str> {
    if root_override.is_some() {
        return Some("--root");
    }
    std::env::var("ORBIT_ROOT")
        .is_ok_and(|value| !value.trim().is_empty())
        .then_some("ORBIT_ROOT")
}

/// The Orbit root this invocation will use, read from argv before clap runs.
///
/// The derived CLI cannot answer this yet: the tree it would parse against
/// is the one the plugin groups still have to be added to. `--root` is the
/// only argument that changes *which* plugins those are, so it is the only
/// one read here, with the same precedence `resolve_generation_root` applies
/// afterwards (`--root`, then `ORBIT_ROOT`, then the host-global root).
fn plugin_root_override() -> Option<std::path::PathBuf> {
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        let arg = arg.to_string_lossy().into_owned();
        if let Some(value) = arg.strip_prefix("--root=") {
            return Some(std::path::PathBuf::from(value));
        }
        if arg == "--root" {
            return args.next().map(std::path::PathBuf::from);
        }
    }
    None
}

/// The `orbit <ns>` groups this host's enabled plugins contribute (§4.6).
///
/// A host with no plugin install directory answers with one `stat` and no
/// store or config read, so the common case pays nothing for a surface it
/// does not use. Any failure below that is this host's plugin problem, not
/// this command's: the built-in CLI must stay usable, so it is logged and
/// the groups are simply absent (§4.9).
fn plugin_cli_groups() -> Vec<orbit_core::adapter::command::PluginCliGroup> {
    let Ok(root) = orbit_core::runtime::resolve_generation_root(plugin_root_override().as_deref())
    else {
        return Vec::new();
    };
    if !root.join("plugins").is_dir() {
        return Vec::new();
    }
    match orbit_core::adapter::command::host_plugin_cli_groups(&root) {
        Ok(groups) => groups,
        Err(error) => {
            tracing::warn!(
                target: "orbit.cli.plugin",
                error = %error,
                "omitting plugin command groups from the CLI surface"
            );
            Vec::new()
        }
    }
}

/// The command tree `orbit` parses argv against: the derived CLI, the given
/// plugin groups, and the global output options.
///
/// `main` and the help goldens both build it here, so a golden pins the help
/// the binary prints rather than the bare derive.
pub(super) fn cli_command(groups: &[orbit_core::adapter::command::PluginCliGroup]) -> Command {
    // Grouped rows share one description column per template derived from the
    // longest name, wrapping to the sink width on a terminal [ORB-19912].
    // Width 0 (piped) keeps single-line rows so help goldens stay
    // deterministic. The width comes from the sink, the only place that
    // queries terminal state (`check-terminal-state-guard.sh`); `main` still
    // resolves its own sink after parsing for rendering.
    let term_width = crate::output::sink::OutputSink::from_process(None, false).width();
    // Clap's own help text (flag descriptions, about, command lists,
    // after-help) wraps to the same width with its hanging indent. `0`, the
    // piped width, tells Clap to keep source formatting rather than assume a
    // terminal; it also overrides Clap's own 100-column ceiling, so a long
    // line wraps at 120 as it does at 60.
    let root =
        plugin_cli::augment(command::Cli::command(), groups).term_width(usize::from(term_width));
    let mut root = root.help_template(crate::help::root_help_template(term_width));
    if !groups.is_empty() {
        // The hand-rolled top-level template lists commands by section, so a
        // plugin group is listed in its own section rather than left out of
        // `orbit --help` entirely.
        root = root.help_template(crate::help::root_help_template(term_width).replace(
            "\nOptions:",
            &format!("{}\nOptions:", plugin_cli::help_section(groups)),
        ));
    }
    root = root.mut_subcommand("run", |cmd| {
        cmd.help_template(crate::help::run_help_template(term_width))
    });
    root = root.mut_subcommand("task", |cmd| {
        cmd.help_template(crate::help::task_help_template(term_width))
    });
    // After `install_output_args`, which adds the global-options footer.
    crate::help::hang_wrap_footers(install_output_args(root, true), usize::from(term_width))
}

/// Parse argv into the derived CLI, the two inputs to mode resolution, and
/// whether `--header` asked for a plain-form header row.
pub(super) fn parse_cli() -> (command::Cli, Option<FormatArg>, bool, bool) {
    let groups = plugin_cli_groups();
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let matches = cli_command(&groups)
        .try_get_matches_from(&args)
        .unwrap_or_else(|err| {
            let (requested, legacy) = usage_error::pre_parse_format(&args);
            let err = usage_error::suggest_help_flag(repair_crew_flag_suggestion(err));
            let err = if usage_error::is_unknown_host_flag(&err) {
                usage_error::suggest_host_command(err, host_ssh_command(&args))
            } else {
                err
            };
            usage_error::exit(err, requested, legacy)
        });
    let (requested, legacy) = requested_output(&matches)
        .unwrap_or_else(|err| usage_error::exit(err, Some(FormatArg::Json), true));
    let cli = match plugin_cli::invocation_from_matches(&groups, &matches) {
        // A plugin group is not a `Commands` variant clap can build, so the
        // two global arguments are read here and the rest of the invocation
        // is the tool call the group reduced to.
        Some(invocation) => command::Cli {
            root: matches.get_one::<std::path::PathBuf>("root").cloned(),
            workspace: matches.get_one::<String>("workspace").cloned(),
            command: command::Commands::PluginGroup(Box::new(invocation)),
        },
        None => command::Cli::from_arg_matches(&matches)
            .unwrap_or_else(|err| usage_error::exit(err, requested, legacy)),
    };
    (cli, requested, legacy, plain_header(&matches))
}
