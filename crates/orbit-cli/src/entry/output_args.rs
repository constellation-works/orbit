use clap::{Arg, ArgMatches, Command};

use crate::command;
use crate::help;
use crate::output::sink::FormatArg;

/// Clap id and long name of the global output-format argument.
const FORMAT_ARG_ID: &str = "format";

/// Clap id and long name of the global JSON shorthand.
pub(super) const JSON_ARG_ID: &str = "json";

/// Clap id and long name of the global plain-form header switch.
const HEADER_ARG_ID: &str = "header";

/// The global `--format`, declared exactly once for the whole CLI.
///
/// Installed beside [`json_arg`] throughout the assembled command tree,
/// including plugin groups, so both options work after a subcommand.
fn format_arg() -> Arg {
    Arg::new(FORMAT_ARG_ID)
        .long(FORMAT_ARG_ID)
        .value_name("MODE")
        .value_parser(clap::value_parser!(FormatArg))
        .help("Output format (default: auto — a table on a terminal, plain text when piped)")
}

/// The global `--json` shorthand, preserving its historical pretty output.
fn json_arg() -> Arg {
    Arg::new(JSON_ARG_ID)
        .long(JSON_ARG_ID)
        .action(clap::ArgAction::SetTrue)
        .help("Output as JSON (shorthand for --format json; always pretty-printed)")
}

/// The global `--header`: label the plain form's columns for a reader.
fn header_arg() -> Arg {
    Arg::new(HEADER_ARG_ID)
        .long(HEADER_ARG_ID)
        .action(clap::ArgAction::SetTrue)
        .help("Start plain output with a tab-separated header row (table output always has one)")
}

/// Whether this command already declares a `--format` of its own.
///
/// `orbit audit export` does, naming its export file's serialization with its
/// own value type. It keeps that meaning; the global flag is simply not
/// offered there. A JSON export also selects a JSON confirmation, while a
/// global output format before `audit` still controls the confirmation mode.
fn declares_format(command: &Command) -> bool {
    command
        .get_arguments()
        .any(|arg| arg.get_long() == Some(FORMAT_ARG_ID))
}

/// Add the output options wherever a local flag does not own their spelling.
/// A plugin-derived `--json` tool-input flag keeps its meaning; output JSON
/// remains available at the root/group or through `--format json` there.
///
/// This walks the tree instead of using `Arg::global`, which would be the
/// obvious spelling but panics here. A global arg is keyed by *id*: clap
/// declines to propagate it into a subcommand that already defines the same id
/// (so `audit export` keeps its own `--format`), but it then propagates the
/// *values* of every global id up and down the whole match tree regardless of
/// type. `orbit audit export --format csv` therefore lands an `ExportFormat`
/// under the root's `format` id, and `orbit --format json audit export` lands a
/// `FormatArg` under the subcommand's — each one a downcast panic in the other
/// reader. Declaring the argument per level keeps every value at the level it
/// was parsed at, where its type is the one that level expects.
pub(super) fn install_output_args(command: Command, is_root: bool) -> Command {
    let subcommands: Vec<String> = command
        .get_subcommands()
        .map(|sub| sub.get_name().to_string())
        .collect();

    let mut global_ids: Vec<_> = command
        .get_arguments()
        .filter(|arg| arg.is_global_set() && matches!(arg.get_id().as_str(), "root" | "workspace"))
        .map(|arg| arg.get_id().clone())
        .collect();
    let mut command = if declares_format(&command) {
        command
    } else {
        global_ids.push(FORMAT_ARG_ID.into());
        command.arg(format_arg())
    };
    if !command
        .get_arguments()
        .any(|arg| arg.get_long() == Some(JSON_ARG_ID))
    {
        global_ids.push(JSON_ARG_ID.into());
        command = command.arg(json_arg());
    }
    if !command
        .get_arguments()
        .any(|arg| arg.get_long() == Some(HEADER_ARG_ID))
    {
        global_ids.push(HEADER_ARG_ID.into());
        command = command.arg(header_arg());
    }
    if !is_root {
        command = help::compact_globals(command, &global_ids);
    }
    // Install inherited arguments before clap propagates them, so their help
    // can differ from the root's. Keep a command's own same-id argument intact.
    let inherited: Vec<_> = command
        .get_arguments()
        .filter(|arg| arg.is_global_set() && global_ids.contains(arg.get_id()))
        .cloned()
        .collect();
    for name in subcommands {
        command = command.mut_subcommand(name, |mut sub| {
            for arg in &inherited {
                if !sub
                    .get_arguments()
                    .any(|local| local.get_id() == arg.get_id())
                {
                    sub = sub.arg(arg.clone());
                }
            }
            install_output_args(sub, false)
        });
    }
    command
}

/// The `--format` value, taken from the deepest level that parsed one.
///
/// A level that owns an unrelated `--format` yields a downcast error rather
/// than a value, which reads here as "no global format was requested".
fn requested_format(matches: &ArgMatches) -> Option<FormatArg> {
    let mut level = matches;
    let mut requested = None;
    loop {
        if let Ok(Some(format)) = level.try_get_one::<FormatArg>(FORMAT_ARG_ID) {
            requested = Some(*format);
        }
        match level.subcommand() {
            Some((_, sub)) => level = sub,
            None => return requested,
        }
    }
}

/// Flags selecting the historical machine-readable output rung.
///
/// `--ops` is here alongside `--json` because it is the same rung wearing a
/// different name: on `task list` and `job list` it selects a narrower record
/// shape and has always forced JSON. Leaving it out would make
/// `orbit task list --ops` render a table on a terminal.
const LEGACY_JSON_ARG_IDS: [&str; 2] = [JSON_ARG_ID, "ops"];

/// Whether `--json` or a command's `--ops` boolean was set at any level.
///
/// Both route through the sink's legacy rung to preserve pretty output.
fn legacy_json(matches: &ArgMatches) -> bool {
    let mut level = matches;
    loop {
        for id in LEGACY_JSON_ARG_IDS {
            if matches!(level.try_get_one::<bool>(id), Ok(Some(true))) {
                return true;
            }
        }
        match level.subcommand() {
            Some((_, sub)) => level = sub,
            None => return false,
        }
    }
}

/// Whether `--header` was set at any level.
pub(super) fn plain_header(matches: &ArgMatches) -> bool {
    let mut level = matches;
    loop {
        if matches!(level.try_get_one::<bool>(HEADER_ARG_ID), Ok(Some(true))) {
            return true;
        }
        match level.subcommand() {
            Some((_, sub)) => level = sub,
            None => return false,
        }
    }
}

/// Resolve explicit output options once, rejecting contradictory spellings.
///
/// `--ops` retains its existing precedence behavior; only the global JSON
/// shorthand conflicts with an explicit non-JSON output format. An explicit
/// audit export format of `json` also requests a JSON confirmation when no
/// global output format was supplied.
pub(super) fn requested_output(
    matches: &ArgMatches,
) -> Result<(Option<FormatArg>, bool), clap::Error> {
    let requested = requested_format(matches).or_else(|| {
        command::audit::export_format_requests_json(matches).then_some(FormatArg::Json)
    });
    let legacy = legacy_json(matches);
    let mut level = matches;
    loop {
        if matches!(level.try_get_one::<bool>(JSON_ARG_ID), Ok(Some(true)))
            && requested.is_some_and(|format| format != FormatArg::Json)
        {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "--json cannot be combined with a non-JSON --format; use --format json",
            ));
        }
        match level.subcommand() {
            Some((_, sub)) => level = sub,
            None => return Ok((requested, legacy)),
        }
    }
}
