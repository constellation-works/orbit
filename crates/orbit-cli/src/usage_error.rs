//! Usage errors reported in the invocation's output mode (output-modes spec §5).
//!
//! clap reports a rejected argv before any matches exist, so the mode cannot
//! come from the parsed `--format` and `--json` the way it does for a command
//! failure. [`pre_parse_format`] reads the same two rungs lexically from argv
//! instead, and the rest of the ladder (`ORBIT_FORMAT`, the sink) resolves
//! exactly as it does after a successful parse.

use std::ffi::OsString;

use clap::ValueEnum;
use clap::error::ErrorKind;

use crate::output::sink::{FormatArg, OutputMode, OutputSink};

/// The `--format` value and whether a legacy `--json`/`--ops` flag appears in
/// argv, read without the command tree.
///
/// The last well-formed `--format <mode>` / `--format=<mode>` wins; a value
/// that is not a mode (a command-local `--format`, such as `audit export`'s
/// `csv`, or the invalid value being reported) is not a request. Nothing after
/// a bare `--` is an option.
pub(crate) fn pre_parse_format(args: &[OsString]) -> (Option<FormatArg>, bool) {
    let mut requested = None;
    let mut legacy = false;
    let mut args = args.iter().skip(1).map(|arg| arg.to_string_lossy());
    while let Some(arg) = args.next() {
        let value = match arg.as_ref() {
            "--" => break,
            "--json" | "--ops" => {
                legacy = true;
                continue;
            }
            "--format" => args.next(),
            other => other
                .strip_prefix("--format=")
                .map(|value| value.to_string().into()),
        };
        if let Some(format) = value.and_then(|value| FormatArg::from_str(&value, false).ok()) {
            requested = Some(format);
        }
    }
    (requested, legacy)
}

/// Report a clap error and exit with clap's exit code.
///
/// In `json`/`ndjson` mode a usage error is the §5 error object on stderr,
/// with code `usage_error`; otherwise clap prints it as it always has. Help and
/// version output are not errors and keep clap's rendering in every mode, as
/// does the help clap prints in place of a missing subcommand — that is a
/// help page, not a message a JSON consumer could act on.
pub(crate) fn exit(error: clap::Error, requested: Option<FormatArg>, legacy_json: bool) -> ! {
    if error.use_stderr() && error.kind() != ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand {
        let sink = OutputSink::from_process(requested, legacy_json);
        if matches!(sink.mode(), OutputMode::Json | OutputMode::Ndjson) {
            let payload = crate::output::json::usage_error_payload(&message(&error));
            if let Ok(rendered) = crate::output::json::render(&payload, sink.pretty_json()) {
                eprintln!("{rendered}");
                orbit_common::observability::logging::exit(error.exit_code());
            }
        }
    }
    // `clap::Error::exit` leaves through `std::process::exit` and would skip
    // the JSONL flush.
    let _ = error.print();
    orbit_common::observability::logging::exit(error.exit_code())
}

/// clap's rendered message without its styling or its `error: ` label, which
/// the JSON object's `error` key already carries.
fn message(error: &clap::Error) -> String {
    let rendered = error.render().to_string();
    let rendered = rendered.trim();
    rendered
        .strip_prefix("error: ")
        .unwrap_or(rendered)
        .to_string()
}

/// Point `orbit help` and `orbit <command> help` at `--help`.
///
/// The `help` subcommand is disabled on purpose, so clap reports it like any
/// unknown word and offers no way forward. `help` is the one word people try
/// first, so the rejection says what to type instead.
pub(crate) fn suggest_help_flag(mut error: clap::Error) -> clap::Error {
    use clap::error::{ContextKind, ContextValue};

    let word = match error.kind() {
        ErrorKind::InvalidSubcommand => error.get(ContextKind::InvalidSubcommand),
        ErrorKind::UnknownArgument => error.get(ContextKind::InvalidArg),
        _ => None,
    };
    if matches!(word, Some(ContextValue::String(word)) if word == "help") {
        error.insert(
            ContextKind::Suggested,
            ContextValue::StyledStrs(vec![clap::builder::StyledStr::from(
                "there is no `help` subcommand; use `--help` (e.g. `orbit <command> --help`)",
            )]),
        );
    }
    error
}

/// Say where a host-local command runs when `--host` is passed to one.
///
/// `--host` routes only id-addressed task calls, `tool run` and
/// `run auto --pull`. Every other command acts on this machine, so the
/// rejection names the command to run on that host instead. `ssh_command` is
/// that command when the host resolved, else `None`.
pub(crate) fn suggest_host_command(
    mut error: clap::Error,
    ssh_command: Option<String>,
) -> clap::Error {
    use clap::error::{ContextKind, ContextValue};

    if !is_unknown_host_flag(&error) {
        return error;
    }
    let hint = match ssh_command {
        Some(command) => format!(
            "this command runs on this machine and takes no `--host`; run it on that host: \
             `{command}`"
        ),
        None => "this command runs on this machine and takes no `--host`; run it on the host \
                 you mean with `ssh <target> orbit …` (see `orbit host list`)"
            .to_string(),
    };
    error.insert(
        ContextKind::Suggested,
        ContextValue::StyledStrs(vec![clap::builder::StyledStr::from(hint)]),
    );
    error
}

/// Whether clap rejected `--host` (or `--host=…`) as an unknown argument.
pub(crate) fn is_unknown_host_flag(error: &clap::Error) -> bool {
    use clap::error::{ContextKind, ContextValue};

    error.kind() == ErrorKind::UnknownArgument
        && matches!(
            error.get(ContextKind::InvalidArg),
            Some(ContextValue::String(arg)) if arg == "--host" || arg.starts_with("--host=")
        )
}
