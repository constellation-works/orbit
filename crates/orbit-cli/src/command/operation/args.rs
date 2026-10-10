//! The clap adapter over the operations-as-data kernel [ORB-10358].
//!
//! ADR-0209 bearing 1: an operation is declared once in `orbit-common` and each
//! surface derives its wiring. This module is the CLI half — it turns an
//! [`OperationSpec`] registry into a clap subcommand tree, and turns the parsed
//! matches back into the tool input the operation takes.
//!
//! Generic over the noun's verb type on purpose: the next noun to migrate
//! reuses every function here and supplies only its own registry. Response
//! rendering stays with the noun, because that is presentation, not contract.
//!
//! # Fidelity to `#[derive(Args)]`
//!
//! These builders reproduce what clap's derive macro generates for an
//! equivalent struct: arg id is the wire field name, value name is that name in
//! SCREAMING_SNAKE, `Vec`-shaped params append with a value delimiter, and args
//! are added in declaration order so `--help` display order is preserved. That
//! fidelity is what lets a hand-written command move to a registry without
//! moving its shipped argv surface.

use clap::error::ErrorKind;
use clap::{Arg, ArgAction, ArgMatches, Command};
use orbit_common::governance::operation::{
    CliArgKind, CliBinding, OperationSpec, ParamSpec, ParamType,
};
use serde_json::{Map, Value};

/// One parsed operation invocation: which verb and its tool input.
pub(crate) struct Invocation<V: 'static> {
    /// The spec that was invoked.
    pub spec: &'static OperationSpec<V>,
    /// Tool input built from the spec's parameters.
    pub input: Value,
}

impl<V: 'static> Invocation<V> {
    /// The audit target id: the positional record id, when the verb takes one.
    pub fn target_id(&self) -> Option<&str> {
        let param = self.spec.cli_positional()?;
        self.input.get(param.name).and_then(Value::as_str)
    }
}

/// Add one subcommand per registry entry, in registry order.
pub(crate) fn augment_subcommands<V: 'static>(
    cmd: Command,
    registry: &'static [OperationSpec<V>],
) -> Command {
    registry
        .iter()
        .fold(cmd, |cmd, spec| cmd.subcommand(subcommand_for(spec)))
}

/// Resolve the invoked subcommand against the registry and project its input.
pub(crate) fn invocation_from_matches<V: 'static>(
    registry: &'static [OperationSpec<V>],
    noun: &str,
    matches: &ArgMatches,
) -> Result<Invocation<V>, clap::Error> {
    let Some((name, sub_matches)) = matches.subcommand() else {
        return Err(clap::Error::raw(
            ErrorKind::MissingSubcommand,
            format!("a {noun} subcommand is required\n"),
        ));
    };
    let Some(spec) = orbit_common::governance::operation::find_by_name(registry, name) else {
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

/// Build one verb's clap subcommand from its spec.
///
/// Arg construction order is the spec's parameter order. Global output
/// options are added to the assembled tree by the CLI entry point.
fn subcommand_for<V: 'static>(spec: &'static OperationSpec<V>) -> Command {
    let mut cmd = Command::new(spec.name).about(spec.cli_about);
    for (param, binding) in spec.cli_params() {
        cmd = cmd.arg(arg_for(param, binding));
    }
    cmd
}

/// Build one clap `Arg` from a parameter spec.
fn arg_for(param: &'static ParamSpec, binding: CliBinding) -> Arg {
    let arg = Arg::new(param.name)
        .help(binding.help.resolve())
        .value_name(static_value_name(param))
        .required(param.required);

    match binding.kind {
        CliArgKind::Positional => arg,
        CliArgKind::Flag { long, delimiter } => {
            let arg = arg.long(long);
            match param.param_type {
                ParamType::String => arg,
                ParamType::StringList => arg.action(ArgAction::Append).value_delimiter(delimiter),
                ParamType::Integer => arg.value_parser(clap::value_parser!(usize)),
                ParamType::Boolean => arg.value_parser(clap::value_parser!(bool)),
            }
        }
    }
}

/// `clap::Arg::value_name` only accepts `&'static str`, and the SCREAMING_SNAKE
/// form is computed from the spec rather than stored twice. The command tree is
/// built once per process and every registry is a fixed-size `&'static` table,
/// so this interns a bounded number of short strings for the process lifetime.
fn static_value_name(param: &'static ParamSpec) -> &'static str {
    Box::leak(param.cli_value_name().into_boxed_str())
}

/// Project parsed args into the verb's tool input.
///
/// Optional string parameters are trimmed and dropped when empty unless the
/// spec declares that empty has meaning, such as clearing a stored value.
/// Omitted arguments stay absent. Required parameters are passed through
/// verbatim and left for the handler to validate, which keeps "you passed only
/// whitespace" reporting where the domain rules live.
fn input_from_matches<V: 'static>(spec: &'static OperationSpec<V>, matches: &ArgMatches) -> Value {
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
            ParamType::Boolean => {
                if let Some(value) = matches.get_one::<bool>(param.name) {
                    input.insert(param.name.to_string(), Value::Bool(*value));
                }
            }
        }
    }
    Value::Object(input)
}
