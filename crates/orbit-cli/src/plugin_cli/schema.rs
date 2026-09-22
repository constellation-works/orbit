//! The schema-to-flag mapping behind `orbit <ns> <verb>` (design §4.6).
//!
//! One rule per JSON Schema shape, applied to the **top level** of a tool's
//! `input_schema` and nowhere deeper:
//!
//! | Property shape | Surface |
//! |---|---|
//! | `string` (with or without `enum`) | `--kebab-case <VALUE>`; an `enum` becomes clap's possible values |
//! | `integer` / `number` | `--kebab-case <N>`, parsed and sent as a JSON number |
//! | `boolean` | `--kebab-case` (true), or `--kebab-case <true\|false>` |
//! | `array` of scalars | `--kebab-case <VALUE>`, repeat once per element |
//! | `object`, `array` of objects, or an untyped property | `--kebab-case-json '<JSON>'` |
//! | named in `cli.positional` | the same value as a positional argument, in manifest order |
//!
//! `--input` / `--input-file` are always accepted and always win: a call
//! that passes either sends exactly that payload, so the long tail a flag
//! cannot express is never out of reach. Nothing here is marked required at
//! the clap level — the tool's own `input_schema` is the authority on what a
//! call must contain, and a flag marked required would make `--input` alone
//! unusable.

use clap::{Arg, ArgAction, ArgMatches, builder::PossibleValuesParser};
use orbit_core::OrbitError;
use orbit_types::plugin::derive_plugin_cli_flag;
use serde_json::{Map, Value};

/// Flags this CLI owns on every plugin subcommand. A property with one of
/// these names keeps its place in the schema and stays reachable through
/// `--input`; it simply gets no flag of its own, because two meanings on one
/// spelling is worse than one missing shortcut.
pub(super) const RESERVED_FLAGS: &[&str] = &[
    "input",
    "input-file",
    "dry-run",
    "format",
    "root",
    "workspace",
    "help",
    "version",
];

/// How one top-level property reaches the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FlagKind {
    Str,
    Integer,
    Number,
    Bool,
    StrList,
    IntegerList,
    NumberList,
    /// Nested shapes: `--<name>-json '<JSON>'`.
    Json,
}

/// One derived argument: the property it fills and how it is spelled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DerivedArg {
    /// The schema property this fills.
    pub(super) property: String,
    /// Long name (`--<long>`), or the positional value name.
    pub(super) long: String,
    pub(super) kind: FlagKind,
    pub(super) description: String,
    pub(super) enum_values: Vec<String>,
    /// Promoted by `cli.positional`.
    pub(super) positional: bool,
}

/// A clap-internal id outside the namespace used by host-owned arguments.
///
/// The raw property remains the key written to tool input, while this prefix
/// prevents properties such as `input` and `root` from colliding with host
/// arguments whose long names differ after JSON-shape derivation.
fn clap_id(derived: &DerivedArg) -> String {
    format!("plugin-input:{}", derived.property)
}

/// Derive every argument of one tool, positional ones first and in the order
/// `cli.positional` names them.
pub(super) fn derive_args(input_schema: &Value, positional: &[String]) -> Vec<DerivedArg> {
    let Some(properties) = input_schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut args: Vec<DerivedArg> = Vec::new();
    for name in positional {
        if let Some(property) = properties.get(name) {
            let mut arg = derive_one(name, property);
            // A nested shape has no readable positional form, so a manifest
            // that promotes one still gets the `--<name>-json` flag.
            arg.positional = arg.kind != FlagKind::Json;
            args.push(arg);
        }
    }
    for (name, property) in properties {
        if positional.iter().any(|promoted| promoted == name) {
            continue;
        }
        args.push(derive_one(name, property));
    }
    let mut long_counts = std::collections::BTreeMap::new();
    for arg in &args {
        *long_counts.entry(arg.long.clone()).or_insert(0_usize) += 1;
    }
    // Loading validates this shape, but an already-registered plugin from an
    // older host must never make clap reject every built-in command. Omit all
    // ambiguous or empty shortcuts; `--input` remains the lossless fallback.
    args.retain(|arg| {
        !arg.long.is_empty()
            && long_counts.get(&arg.long) == Some(&1)
            && !RESERVED_FLAGS.contains(&arg.long.as_str())
    });
    args
}

fn derive_one(name: &str, property: &Value) -> DerivedArg {
    let kind = flag_kind(property);
    let long = derive_plugin_cli_flag(name, property);
    DerivedArg {
        property: name.to_string(),
        long,
        kind,
        description: property
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        enum_values: property
            .get("enum")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        positional: false,
    }
}

fn flag_kind(property: &Value) -> FlagKind {
    match property.get("type").and_then(Value::as_str) {
        Some("string") => FlagKind::Str,
        Some("integer") => FlagKind::Integer,
        Some("number") => FlagKind::Number,
        Some("boolean") => FlagKind::Bool,
        Some("array") => match property
            .get("items")
            .and_then(|items| items.get("type"))
            .and_then(Value::as_str)
        {
            Some("string") => FlagKind::StrList,
            Some("integer") => FlagKind::IntegerList,
            Some("number") => FlagKind::NumberList,
            _ => FlagKind::Json,
        },
        _ => FlagKind::Json,
    }
}

/// The clap argument for one derived property.
pub(super) fn clap_arg(derived: &DerivedArg) -> Arg {
    let mut arg = Arg::new(clap_id(derived));
    if derived.positional {
        arg = arg.value_name(derived.long.to_uppercase()).required(false);
    } else {
        arg = arg
            .long(derived.long.clone())
            .value_name(match derived.kind {
                FlagKind::Json => "JSON",
                FlagKind::Bool => "BOOL",
                FlagKind::Integer | FlagKind::IntegerList => "N",
                FlagKind::Number | FlagKind::NumberList => "NUMBER",
                _ => "VALUE",
            });
    }
    if !derived.description.trim().is_empty() {
        arg = arg.help(derived.description.trim().to_string());
    }
    if matches!(derived.kind, FlagKind::Str | FlagKind::StrList) && !derived.enum_values.is_empty()
    {
        // An `enum` in the schema is the tool's own closed set, so clap
        // refuses an unknown value with the list rather than sending it to
        // the backend to be rejected there.
        arg = arg.value_parser(PossibleValuesParser::new(derived.enum_values.clone()));
        if matches!(derived.kind, FlagKind::StrList) && !derived.positional {
            arg = arg.action(ArgAction::Append);
        }
        return arg;
    }
    match derived.kind {
        FlagKind::Bool if !derived.positional => {
            // `--flag` means true; `--flag false` is still available for a
            // property whose schema default is true.
            arg.num_args(0..=1)
                .default_missing_value("true")
                .value_parser(clap::value_parser!(bool))
        }
        FlagKind::Bool => arg.value_parser(clap::value_parser!(bool)),
        FlagKind::Integer | FlagKind::IntegerList => {
            let arg = arg.value_parser(clap::value_parser!(i64));
            if matches!(derived.kind, FlagKind::IntegerList) && !derived.positional {
                arg.action(ArgAction::Append)
            } else {
                arg
            }
        }
        FlagKind::Number | FlagKind::NumberList => {
            let arg = arg.value_parser(clap::value_parser!(f64));
            if matches!(derived.kind, FlagKind::NumberList) && !derived.positional {
                arg.action(ArgAction::Append)
            } else {
                arg
            }
        }
        FlagKind::StrList if !derived.positional => arg.action(ArgAction::Append),
        _ => arg,
    }
}

/// Collect the parsed arguments into the tool input this call sends.
pub(super) fn input_from_matches(
    args: &[DerivedArg],
    matches: &ArgMatches,
) -> Result<Value, OrbitError> {
    let mut object = Map::new();
    for arg in args {
        let id = clap_id(arg);
        let value = match arg.kind {
            FlagKind::Str => matches
                .try_get_one::<String>(&id)
                .ok()
                .flatten()
                .map(|value| Value::String(value.clone())),
            FlagKind::Bool => matches
                .try_get_one::<bool>(&id)
                .ok()
                .flatten()
                .map(|value| Value::Bool(*value)),
            FlagKind::Integer => matches
                .try_get_one::<i64>(&id)
                .ok()
                .flatten()
                .map(|value| Value::Number((*value).into())),
            FlagKind::Number => matches
                .try_get_one::<f64>(&id)
                .ok()
                .flatten()
                .and_then(|value| serde_json::Number::from_f64(*value).map(Value::Number)),
            FlagKind::StrList => matches
                .try_get_many::<String>(&id)
                .ok()
                .flatten()
                .map(|values| {
                    Value::Array(values.map(|value| Value::String(value.clone())).collect())
                }),
            FlagKind::IntegerList => {
                matches
                    .try_get_many::<i64>(&id)
                    .ok()
                    .flatten()
                    .map(|values| {
                        Value::Array(values.map(|value| Value::Number((*value).into())).collect())
                    })
            }
            FlagKind::NumberList => matches
                .try_get_many::<f64>(&id)
                .ok()
                .flatten()
                .map(|values| {
                    Value::Array(
                        values
                            .filter_map(|value| {
                                serde_json::Number::from_f64(*value).map(Value::Number)
                            })
                            .collect(),
                    )
                }),
            FlagKind::Json => match matches.try_get_one::<String>(&id).ok().flatten() {
                Some(raw) => Some(serde_json::from_str(raw).map_err(|error| {
                    OrbitError::InvalidInput(format!("--{} is not valid JSON: {error}", arg.long))
                })?),
                None => None,
            },
        };
        if let Some(value) = value {
            object.insert(arg.property.clone(), value);
        }
    }
    Ok(Value::Object(object))
}
