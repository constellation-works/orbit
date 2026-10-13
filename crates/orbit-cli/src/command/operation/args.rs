//! The clap adapter over the operations-as-data kernel [ORB-10358].
//!
//! ADR-0209 bearing 1: an operation is declared once in `orbit-common` and each
//! surface derives its wiring. This module is the CLI half — it turns an
//! [`OperationSpec`] registry into a clap subcommand tree, and turns the parsed
//! matches back into the tool input the operation takes.
//!
//! Generic over the noun's verb type on purpose: [`Invocation<V>`] implements
//! clap's `Subcommand` and `FromArgMatches` for every [`OperationVerb`], so a
//! migrated noun writes no clap impls and supplies only its registry. Response
//! rendering stays with the noun, as an exhaustive match on its verb, because
//! only the CLI needs it.
//!
//! # Fidelity to `#[derive(Args)]`
//!
//! These builders reproduce what clap's derive macro generates for an
//! equivalent struct: arg id is the wire field name, value name is that name in
//! SCREAMING_SNAKE, `Vec`-shaped params append with a value delimiter, and args
//! are added in `--help` order (declaration order unless the spec's
//! [`CliHelp::flag_order`](orbit_common::governance::operation::CliHelp::flag_order)
//! says otherwise) so display order is preserved. That fidelity is what lets a
//! hand-written command move to a registry without moving its shipped argv
//! surface.

use clap::builder::{PossibleValue, PossibleValuesParser};
use clap::error::ErrorKind;
use clap::{Arg, ArgAction, ArgMatches, Command, FromArgMatches, Subcommand};
use orbit_common::governance::operation::{
    CliArgKind, CliBinding, EnumValue, OperationSpec, OperationVerb, ParamSpec, ParamType,
    alternatives,
};
use orbit_core::OrbitError;
use serde_json::{Map, Value};

/// One parsed operation invocation: which verb and its tool input.
pub(crate) struct Invocation<V: 'static> {
    /// The spec that was invoked.
    pub spec: &'static OperationSpec<V>,
    /// Tool input built from the spec's parameters.
    pub input: Map<String, Value>,
}

impl<V: OperationVerb> Invocation<V> {
    /// The audit target id: the noun's target parameter, when the verb takes
    /// it.
    pub fn target_id(&self) -> Option<&str> {
        self.input
            .get(V::noun().audit_target_param)
            .and_then(Value::as_str)
    }

    /// The audit `subcommand` label, derived from the CLI path.
    pub fn audit_subcommand(&self) -> String {
        self.spec.cli_path.join("-")
    }

    /// Refuse an invocation that sets none of the spec's `at_least_one_of`
    /// parameters, naming the flags that would.
    ///
    /// The tool entry enforces the same rule but names input fields, which is
    /// right for MCP and wrong for a person at a prompt.
    pub fn require_at_least_one_of(&self) -> Result<(), OrbitError> {
        if !self.spec.misses_at_least_one_of(&self.input) {
            return Ok(());
        }
        let flags = self.spec.at_least_one_of.iter().map(|field| {
            let binding = self
                .spec
                .cli_params()
                .find(|(param, _)| param.name == *field)
                .map(|(_, binding)| binding.kind);
            match binding {
                Some(CliArgKind::Flag { long, .. }) => format!("`--{long}`"),
                Some(CliArgKind::Positional) | None => format!("`{field}`"),
            }
        });
        Err(OrbitError::InvalidInput(format!(
            "nothing to {}; pass at least one of {}",
            self.spec.cli_path.join(" "),
            alternatives(flags)
        )))
    }
}

impl<V: OperationVerb> Subcommand for Invocation<V> {
    fn augment_subcommands(cmd: Command) -> Command {
        V::noun()
            .operations
            .iter()
            .fold(cmd, |cmd, spec| cmd.subcommand(subcommand_for(spec)))
    }

    fn augment_subcommands_for_update(cmd: Command) -> Command {
        <Self as Subcommand>::augment_subcommands(cmd)
    }

    fn has_subcommand(name: &str) -> bool {
        find_by_subcommand::<V>(name).is_some()
    }
}

impl<V: OperationVerb> FromArgMatches for Invocation<V> {
    fn from_arg_matches(matches: &ArgMatches) -> Result<Self, clap::Error> {
        let noun = V::noun().cli_name;
        let Some((name, sub_matches)) = matches.subcommand() else {
            return Err(clap::Error::raw(
                ErrorKind::MissingSubcommand,
                format!("a {noun} subcommand is required\n"),
            ));
        };
        let Some(spec) = find_by_subcommand::<V>(name) else {
            return Err(clap::Error::raw(
                ErrorKind::InvalidSubcommand,
                format!("unrecognized {noun} subcommand `{name}`\n"),
            ));
        };
        Ok(Invocation {
            input: input_from_matches(spec, sub_matches),
            spec,
        })
    }

    fn update_from_arg_matches(&mut self, matches: &ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

/// The operation whose CLI subcommand is `name`.
fn find_by_subcommand<V: OperationVerb>(name: &str) -> Option<&'static OperationSpec<V>> {
    V::noun()
        .operations
        .iter()
        .find(|spec| spec.cli_path == [name])
}

/// Build one verb's clap subcommand from its spec.
///
/// Arg construction order is the spec's parameter order. Global output
/// options are added to the assembled tree by the CLI entry point.
///
/// Every path is one segment until a noun declares
/// [`NounSpec::cli_groups`](orbit_common::governance::operation::NounSpec::cli_groups);
/// the group adapter lands with that noun.
fn subcommand_for<V: 'static>(spec: &'static OperationSpec<V>) -> Command {
    debug_assert_eq!(
        spec.cli_path.len(),
        1,
        "{} declares a nested CLI path, which needs the CLI group adapter",
        spec.tool_name
    );
    let mut cmd = Command::new(spec.cli_path.join("-")).about(spec.cli_about);
    if let Some(long_about) = spec.cli_help.long_about {
        cmd = cmd.long_about(long_about);
    }
    if let Some(after_help) = spec.cli_help.after_help {
        cmd = cmd.after_help(after_help);
    }
    for (param, binding) in spec.cli_params_in_help_order() {
        cmd = cmd.arg(arg_for(param, binding));
    }
    cmd
}

/// Build one clap `Arg` from a parameter spec.
fn arg_for(param: &'static ParamSpec, binding: CliBinding) -> Arg {
    let mut arg = Arg::new(param.name)
        .help(binding.help.resolve())
        .required(param.required);
    if let Some(default) = param.default {
        arg = arg.default_value(default);
    }
    let mut delimiter = None;
    if let CliArgKind::Flag {
        long,
        delimiter: flag_delimiter,
        aliases,
        visible_aliases,
        value_name: _,
        multiple,
    } = binding.kind
    {
        delimiter = flag_delimiter;
        arg = arg
            .long(long)
            .aliases(aliases.iter().copied())
            .visible_aliases(visible_aliases.iter().copied());
        if multiple {
            arg = arg.num_args(1..);
        }
    }
    match param.param_type {
        // A presence flag takes no value, so it has no value name.
        ParamType::Switch => return arg.action(ArgAction::SetTrue),
        ParamType::String | ParamType::Object => {}
        ParamType::StringList => arg = arg.action(ArgAction::Append).value_delimiter(delimiter),
        ParamType::Integer => arg = arg.value_parser(clap::value_parser!(usize)),
        ParamType::UnsignedInteger => arg = arg.value_parser(clap::value_parser!(u64)),
        ParamType::Boolean => arg = arg.value_parser(clap::value_parser!(bool)),
        ParamType::Enum(values) => {
            arg = arg.value_parser(PossibleValuesParser::new(values.iter().map(possible_value)))
        }
        ParamType::EnumList(values) => {
            arg = arg
                .action(ArgAction::Append)
                .value_delimiter(delimiter)
                .value_parser(PossibleValuesParser::new(values.iter().map(possible_value)))
        }
    }
    arg.value_name(param.cli_value_name())
}

fn possible_value(value: &EnumValue) -> PossibleValue {
    let possible = PossibleValue::new(value.name).aliases(value.aliases.iter().copied());
    match value.help {
        Some(help) => possible.help(help),
        None => possible,
    }
}

/// The advertised name of the enum value `raw` spells, which may be an alias.
fn canonical_enum_value(values: &[EnumValue], raw: &str) -> String {
    values
        .iter()
        .find(|value| value.name == raw || value.aliases.contains(&raw))
        .map_or(raw, |value| value.name)
        .to_string()
}

/// Project parsed args into the verb's tool input.
///
/// Optional string parameters are trimmed and dropped when empty unless the
/// spec declares that empty has meaning, such as clearing a stored value.
/// Omitted arguments stay absent. Required parameters are passed through
/// verbatim and left for the handler to validate, which keeps "you passed only
/// whitespace" reporting where the domain rules live.
fn input_from_matches<V: 'static>(
    spec: &'static OperationSpec<V>,
    matches: &ArgMatches,
) -> Map<String, Value> {
    let mut input = Map::new();
    for (param, _binding) in spec.cli_params() {
        match param.param_type {
            ParamType::String => {
                let Some(value) = matches.get_one::<String>(param.name) else {
                    continue;
                };
                if param.required {
                    input.insert(param.name.to_string(), Value::String(value.clone()));
                } else if param.preserve_empty || !value.trim().is_empty() {
                    input.insert(
                        param.name.to_string(),
                        Value::String(value.trim().to_string()),
                    );
                }
            }
            ParamType::StringList => {
                let values = matches
                    .get_many::<String>(param.name)
                    .into_iter()
                    .flatten()
                    .map(|value| value.trim())
                    .filter(|value| !value.is_empty())
                    .map(|value| Value::String(value.to_string()))
                    .collect::<Vec<_>>();
                if !values.is_empty() {
                    input.insert(param.name.to_string(), Value::Array(values));
                }
            }
            ParamType::Integer => {
                if let Some(value) = matches.get_one::<usize>(param.name) {
                    input.insert(param.name.to_string(), Value::from(*value));
                }
            }
            ParamType::UnsignedInteger => {
                if let Some(value) = matches.get_one::<u64>(param.name) {
                    input.insert(param.name.to_string(), Value::from(*value));
                }
            }
            ParamType::Boolean => {
                if let Some(value) = matches.get_one::<bool>(param.name) {
                    input.insert(param.name.to_string(), Value::Bool(*value));
                }
            }
            ParamType::Switch => {
                if matches.get_flag(param.name) {
                    input.insert(param.name.to_string(), Value::Bool(true));
                }
            }
            ParamType::Enum(values) => {
                if let Some(value) = matches.get_one::<String>(param.name) {
                    input.insert(
                        param.name.to_string(),
                        Value::String(canonical_enum_value(values, value)),
                    );
                }
            }
            ParamType::EnumList(values) => {
                let values = matches
                    .get_many::<String>(param.name)
                    .into_iter()
                    .flatten()
                    .map(|value| Value::String(canonical_enum_value(values, value)))
                    .collect::<Vec<_>>();
                if !values.is_empty() {
                    input.insert(param.name.to_string(), Value::Array(values));
                }
            }
            // A JSON argument. Text that does not parse is passed as a string
            // for the tool entry's type check to refuse.
            ParamType::Object => {
                if let Some(value) = matches.get_one::<String>(param.name) {
                    input.insert(
                        param.name.to_string(),
                        serde_json::from_str(value)
                            .unwrap_or_else(|_| Value::String(value.clone())),
                    );
                }
            }
        }
    }
    input
}
