//! The global `--format` argument's surface.
//!
//! One declaration ([`crate::format_arg`]) reaches every command that does not
//! own a `--format` of its own, and the one that does is left alone.

use clap::{ArgMatches, Command, CommandFactory, FromArgMatches};

use crate::command::Cli;
use crate::output::sink::FormatArg;
use crate::{FORMAT_ARG_ID, install_format_arg, requested_format};

/// Commands that declared `--format` before the global one existed, with
/// their own value types. Adding to this list is a decision, not an accident:
/// a command here does not accept the global flag.
const COMMANDS_OWNING_FORMAT: &[&str] = &["orbit audit export"];

/// Our argument's value name, which distinguishes it from a command's own
/// `--format` when walking the tree.
const GLOBAL_FORMAT_VALUE_NAME: &str = "MODE";

fn parser() -> Command {
    install_format_arg(Cli::command())
}

fn matches(argv: &[&str]) -> ArgMatches {
    parser()
        .try_get_matches_from(argv)
        .unwrap_or_else(|err| panic!("{argv:?} should parse: {err}"))
}

#[test]
fn root_help_lists_the_format_argument_with_its_modes() {
    let help = parser().render_help().to_string();

    assert!(help.contains("--format <MODE>"), "root help:\n{help}");
    assert!(
        help.contains("[possible values: auto, table, json, ndjson]"),
        "root help:\n{help}"
    );
}

#[test]
fn format_is_accepted_before_and_after_the_subcommand() {
    assert_eq!(
        requested_format(&matches(&["orbit", "--format", "json", "task", "list"])),
        Some(FormatArg::Json)
    );
    assert_eq!(
        requested_format(&matches(&["orbit", "task", "list", "--format", "ndjson"])),
        Some(FormatArg::Ndjson)
    );
    assert_eq!(
        requested_format(&matches(&["orbit", "task", "--format", "table", "list"])),
        Some(FormatArg::Table)
    );
}

#[test]
fn absent_format_resolves_to_none_rather_than_a_default() {
    assert_eq!(requested_format(&matches(&["orbit", "task", "list"])), None);
}

#[test]
fn the_deepest_format_wins() {
    let matches = matches(&[
        "orbit", "--format", "json", "task", "list", "--format", "table",
    ]);

    assert_eq!(requested_format(&matches), Some(FormatArg::Table));
}

#[test]
fn a_command_owning_format_keeps_its_own_value() {
    let matches = matches(&[
        "orbit",
        "audit",
        "export",
        "--format",
        "csv",
        "--output",
        "events.csv",
    ]);

    assert_eq!(
        requested_format(&matches),
        None,
        "`csv` belongs to audit export, not to the global flag"
    );
    // Reading a value whose type the level does not expect is a panic inside
    // clap, not an `Err` — so this call is the regression guard for
    // cross-level value bleed, not the assertion below it.
    assert!(Cli::from_arg_matches(&matches).is_ok());
}

#[test]
fn a_global_format_does_not_corrupt_a_command_owning_format() {
    let matches = matches(&[
        "orbit",
        "--format",
        "json",
        "audit",
        "export",
        "--output",
        "events.csv",
    ]);

    assert_eq!(requested_format(&matches), Some(FormatArg::Json));
    assert!(Cli::from_arg_matches(&matches).is_ok());
}

/// [ORB-12113] A command that shadows the global flag has to say so: the same
/// `--format` accepts `ndjson` everywhere else and rejects it here, with a
/// different vocabulary in the error.
#[test]
fn a_command_owning_format_documents_the_shadowing_in_its_help() {
    let help = parser()
        .find_subcommand_mut("audit")
        .and_then(|audit| audit.find_subcommand_mut("export"))
        .expect("audit export")
        .render_long_help()
        .to_string();

    assert!(
        help.contains("shadows the global"),
        "audit export help:\n{help}"
    );
    assert!(
        help.contains("auto|table|json|ndjson"),
        "audit export help must name the vocabulary it shadows:\n{help}"
    );
}

#[test]
fn every_command_declares_exactly_one_format_and_only_the_listed_commands_own_theirs() {
    let mut owners = Vec::new();
    walk(&parser(), "orbit", &mut owners);
    owners.sort();
    let owners: Vec<&str> = owners.iter().map(String::as_str).collect();

    assert_eq!(owners, COMMANDS_OWNING_FORMAT);
}

/// Assert one `--format` per command, recording the commands whose `--format`
/// is not the global one.
fn walk(command: &Command, path: &str, owners: &mut Vec<String>) {
    let declared: Vec<_> = command
        .get_arguments()
        .filter(|arg| arg.get_long() == Some(FORMAT_ARG_ID))
        .collect();

    assert_eq!(
        declared.len(),
        1,
        "`{path}` declares {} `--format` arguments; expected exactly one",
        declared.len()
    );

    let is_global_flag = declared[0]
        .get_value_names()
        .is_some_and(|names| names.len() == 1 && names[0] == GLOBAL_FORMAT_VALUE_NAME);
    if !is_global_flag {
        owners.push(path.to_string());
    }

    for sub in command.get_subcommands() {
        walk(sub, &format!("{path} {}", sub.get_name()), owners);
    }
}
