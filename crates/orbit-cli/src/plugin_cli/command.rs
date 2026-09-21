//! `orbit <ns> <verb>`: one clap group per active plugin, built at startup
//! from the installed manifests (design §4.6).
//!
//! The group is a spelling, never a second execution path. Every invocation
//! becomes the [`ToolRunArgs`] that `orbit tool run <ns>.<verb>` would have
//! built, so dispatch, authorization, dry-run and the audit row are the same
//! ones — `orbit graph recommend --query …` and `orbit tool run
//! graph.recommend --input '{"query":…}'` are one audited operation with two
//! spellings.
//!
//! There is no `git-foo` passthrough: a word that is not an installed,
//! enabled plugin namespace is still clap's unknown-subcommand error.

use std::sync::OnceLock;

use clap::{Arg, ArgMatches, Command};
use orbit_core::adapter::command::{PluginCliGroup, PluginCliVerb};
use orbit_core::{OrbitError, OrbitRuntime};

use super::schema::{DerivedArg, clap_arg, derive_args, input_from_matches};
use crate::command::tool::ToolRunArgs;
use crate::command::{CommandOut, Execute};

/// Help heading `orbit --help` lists plugin groups under.
pub(crate) const PLUGIN_HELP_HEADING: &str = "Plugins:";

/// One parsed `orbit <ns> <verb>` invocation, already reduced to the tool
/// call it performs.
pub struct PluginGroupInvocation {
    /// The plugin namespace this invocation came from. Not read by dispatch
    /// — the tool name below is what runs — but it is what the unit tests
    /// assert the routing on, and what a future diagnostic would name.
    #[cfg_attr(not(test), allow(dead_code))]
    pub namespace: String,
    #[cfg_attr(not(test), allow(dead_code))]
    pub verb: String,
    /// Exactly the arguments `orbit tool run` would have been given.
    pub tool_run: ToolRunArgs,
    /// A flag value this call could not turn into tool input — a malformed
    /// `--<name>-json`. Carried rather than reported at parse time so it
    /// reaches the operator through the same error path, and the same audit
    /// row, as any other bad tool input.
    pub input_error: Option<String>,
}

impl Execute for PluginGroupInvocation {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        if let Some(message) = self.input_error {
            return Err(OrbitError::InvalidInput(message));
        }
        self.tool_run.execute(runtime)
    }
}

/// Add one subcommand group per active plugin.
pub(crate) fn augment(command: Command, groups: &[PluginCliGroup]) -> Command {
    let mut command = command;
    for group in groups {
        command = command.subcommand(group_command(group));
    }
    command
}

fn group_command(group: &PluginCliGroup) -> Command {
    let about = if group.description.trim().is_empty() {
        format!("Tools from the '{}' plugin", group.namespace)
    } else {
        group.description.trim().to_string()
    };
    let mut command = Command::new(group.namespace.clone())
        .about(about)
        .long_about(format!(
            "{}\n\nProvided by the '{}' plugin v{}. Every verb here is the same audited \
             operation as `orbit tool run {}.<verb>`.",
            if group.description.trim().is_empty() {
                format!("Tools from the '{}' plugin", group.namespace)
            } else {
                group.description.trim().to_string()
            },
            group.namespace,
            group.version,
            group.namespace,
        ))
        .subcommand_required(true)
        .arg_required_else_help(true);
    for verb in &group.verbs {
        command = command.subcommand(verb_command(verb));
    }
    command
}

fn verb_command(verb: &PluginCliVerb) -> Command {
    let mut command = Command::new(verb.verb.clone());
    if !verb.description.trim().is_empty() {
        command = command.about(verb.description.trim().to_string());
    }
    command = command.long_about(format!(
        "{}\n\nRuns `{}`{}. `--input` and `--input-file` are always accepted and override \
         every flag.",
        if verb.description.trim().is_empty() {
            format!("Run the {} tool.", verb.tool_name)
        } else {
            verb.description.trim().to_string()
        },
        verb.tool_name,
        if verb.mutating {
            ", a mutating plugin tool"
        } else {
            ", a read-only plugin tool"
        },
    ));
    for derived in derive_args(&verb.input_schema, &verb.positional) {
        command = command.arg(clap_arg(&derived));
    }
    command
        .arg(
            Arg::new("input")
                .long("input")
                .value_name("JSON")
                .help("JSON input for the tool; overrides every derived flag"),
        )
        .arg(
            Arg::new("input-file")
                .long("input-file")
                .value_name("PATH")
                .conflicts_with("input")
                .help("Path to a JSON file to use as input; overrides every derived flag"),
        )
        .arg(
            Arg::new("dry-run")
                .long("dry-run")
                .action(clap::ArgAction::SetTrue)
                .help("Validate without executing"),
        )
}

/// Resolve a top-level match against the plugin groups, and reduce it to the
/// tool call it performs. `None` means the invoked subcommand is not a
/// plugin group, which leaves the ordinary derived CLI to answer.
pub(crate) fn invocation_from_matches(
    groups: &[PluginCliGroup],
    matches: &ArgMatches,
) -> Option<PluginGroupInvocation> {
    let (name, group_matches) = matches.subcommand()?;
    let group = groups.iter().find(|group| group.namespace == name)?;
    let (verb_name, verb_matches) = group_matches.subcommand()?;
    let verb = group.verbs.iter().find(|verb| verb.verb == verb_name)?;
    Some(build_invocation(group, verb, verb_matches))
}

fn build_invocation(
    group: &PluginCliGroup,
    verb: &PluginCliVerb,
    matches: &ArgMatches,
) -> PluginGroupInvocation {
    let explicit_input = matches
        .try_get_one::<String>("input")
        .ok()
        .flatten()
        .cloned();
    let input_file = matches
        .try_get_one::<String>("input-file")
        .ok()
        .flatten()
        .cloned();
    let derived: Vec<DerivedArg> = derive_args(&verb.input_schema, &verb.positional);
    // `--input` / `--input-file` win outright (§4.6): a caller reaching for
    // the escape hatch means the payload it names, not that payload merged
    // with whatever flags happened to be spelled beside it.
    let mut input_error = None;
    let input = match (&explicit_input, &input_file) {
        (Some(raw), _) => Some(raw.clone()),
        (None, Some(_)) => None,
        (None, None) => match input_from_matches(&derived, matches).and_then(|value| {
            serde_json::to_string(&value).map_err(|error| {
                OrbitError::InvalidInput(format!("serialize plugin tool input: {error}"))
            })
        }) {
            Ok(input) => Some(input),
            Err(error) => {
                input_error = Some(error.to_string());
                None
            }
        },
    };
    PluginGroupInvocation {
        namespace: group.namespace.clone(),
        verb: verb.verb.clone(),
        tool_run: ToolRunArgs {
            name: verb.tool_name.clone(),
            input,
            input_file,
            agent: None,
            model: None,
            dry_run: matches
                .try_get_one::<bool>("dry-run")
                .ok()
                .flatten()
                .copied()
                .unwrap_or(false),
            fields: Vec::new(),
            full: false,
            pretty: false,
            parsed_input: OnceLock::new(),
        },
        input_error,
    }
}

/// The `Plugins:` block `orbit --help` prints, or an empty string when this
/// host has no active plugin.
pub(crate) fn help_section(groups: &[PluginCliGroup]) -> String {
    if groups.is_empty() {
        return String::new();
    }
    let width = groups
        .iter()
        .map(|group| group.namespace.len())
        .max()
        .unwrap_or(0)
        .max(11);
    let mut section = format!("\n{PLUGIN_HELP_HEADING}\n");
    for group in groups {
        let about = if group.description.trim().is_empty() {
            format!("Tools from the '{}' plugin", group.namespace)
        } else {
            group.description.trim().to_string()
        };
        section.push_str(&format!(
            "  {:<width$} {about}\n",
            group.namespace,
            width = width
        ));
    }
    section
}
