//! Compact inherited options without changing a command's own help.

use clap::{Command, Id};

/// Long help always puts argument descriptions on separate lines in clap.
/// Render the inherited options as a compact reference instead, ahead of any
/// existing footer. Root help remains the source of their full descriptions.
pub(crate) fn compact_globals(mut command: Command, global_ids: &[Id]) -> Command {
    let mut section = String::from("Global options (see 'orbit --help' for descriptions):");
    for id in global_ids {
        if let Some(arg) = command.get_arguments().find(|arg| arg.get_id() == id) {
            section.push_str(&format!("\n  --{}", arg.get_long().unwrap_or(id.as_str())));
            if arg.get_action().takes_values() {
                let value = arg
                    .get_value_names()
                    .and_then(|names| names.first())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| id.as_str().to_uppercase());
                section.push_str(&format!(" <{value}>"));
            }
        }
    }
    let short = match command.get_after_help() {
        Some(footer) => format!("{section}\n\n{footer}"),
        None => section.clone(),
    };
    let long = command
        .get_after_long_help()
        .map(|footer| format!("{section}\n\n{footer}"));
    for id in global_ids {
        // Keep the flags in usage and suggestions; only their help is custom.
        command = command.mut_arg(id, |arg| arg.hide_short_help(true).hide_long_help(true));
    }
    command = command.after_help(short);
    if let Some(long) = long {
        command = command.after_long_help(long);
    }
    command
}
