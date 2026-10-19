use orbit_types::tool::McpToolScope;
use serde_json::{Map, Value};

use crate::OrbitError;
use crate::protocol::tool_input::{
    TOOL_INPUT_IDENTITY_KEYS, TOOL_INPUT_TRANSPORT_WRAPPER_KEYS, missing_tool_field,
    reject_unknown_tool_fields,
};

use super::input::non_negative_integer;
use super::{
    CliBinding, CliHelp, Description, Effect, OperationInput, ParamSpec, ParamType, RetiredField,
    UnknownFields,
};

/// One verb on one noun, declared once.
///
/// `V` is the noun's verb enum. Keeping it typed (rather than a bare string)
/// is what lets `orbit-core`'s handler table be exhaustiveness-checked against
/// this registry.
#[derive(Debug, Clone, Copy)]
pub struct OperationSpec<V: 'static> {
    /// The typed verb this spec describes.
    pub verb: V,
    /// The CLI subcommand path below the noun (`["add"]`). Also the source of
    /// the audit `subcommand` label.
    pub cli_path: &'static [&'static str],
    /// The fully-qualified tool name (`orbit.friction.add`).
    pub tool_name: &'static str,
    /// MCP tool description.
    pub tool_description: &'static str,
    /// `--help` summary for the CLI subcommand.
    pub cli_about: &'static str,
    /// The rest of the CLI subcommand's `--help`.
    pub cli_help: CliHelp,
    /// Whether the operation reads or writes.
    pub effect: Effect,
    /// Parameter groups, concatenated in order. Order is contract: it drives
    /// MCP schema order, and CLI `--help` order unless
    /// [`CliHelp::flag_order`] overrides it. A group lets verbs share a
    /// run of parameters without functional record update, which `const` does
    /// not allow.
    pub params: &'static [&'static [ParamSpec]],
    /// Parameters of which a call must set at least one, such as the edits an
    /// `update` applies. Empty when the operation has no such rule.
    pub at_least_one_of: &'static [&'static str],
    /// Fields the operation used to accept and now refuses, naming where each
    /// went.
    pub retired_fields: &'static [RetiredField],
    /// What a call that carries an undeclared field gets.
    pub unknown_fields: UnknownFields,
    /// MCP scope when advertised. `None` keeps the operation off MCP while it
    /// remains reachable through its other registered surfaces.
    ///
    /// This is placement, not permission: it decides who is *shown* the verb,
    /// never who may perform it. Authorization is
    /// [`crate::governance::authorization::GOVERNED_OPERATIONS`] alone — see that module's
    /// "Placement is not permission" section.
    pub mcp_scope: Option<McpToolScope>,
}

impl<V: 'static> OperationSpec<V> {
    /// Every parameter, in declaration order across its groups.
    pub fn params(&self) -> impl Iterator<Item = &'static ParamSpec> {
        self.params.iter().flat_map(|group| group.iter())
    }

    /// Parameters the tool accepts: every parameter except the CLI-only ones.
    pub fn wire_params(&self) -> impl Iterator<Item = &'static ParamSpec> {
        self.params().filter(|param| !param.is_cli_only())
    }

    /// Parameters that MCP advertises, in declaration order.
    pub fn mcp_params(&self) -> impl Iterator<Item = (&'static ParamSpec, Description)> {
        self.params().filter_map(|param| {
            param
                .mcp_description
                .map(|description| (param, description))
        })
    }

    /// Parameters bound to the command line, in declaration order.
    pub fn cli_params(&self) -> impl Iterator<Item = (&'static ParamSpec, CliBinding)> {
        self.params()
            .filter_map(|param| param.cli.map(|binding| (param, binding)))
    }

    /// Parameters bound to the command line, in `--help` order: the
    /// [`CliHelp::flag_order`] when the spec declares one, else declaration
    /// order.
    pub fn cli_params_in_help_order(&self) -> Vec<(&'static ParamSpec, CliBinding)> {
        let mut params = self.cli_params().collect::<Vec<_>>();
        let order = self.cli_help.flag_order;
        if !order.is_empty() {
            params.sort_by_key(|(param, _)| {
                order
                    .iter()
                    .position(|name| *name == param.name)
                    .unwrap_or(order.len())
            });
        }
        params
    }

    /// Whether `field` is an input key this operation does not declare.
    ///
    /// A key is declared when it is a parameter name or alias, a retired
    /// field, a transport wrapper (`workspace`, `_meta`) or an attribution key
    /// (`model`, `agent`) that the tool boundary reads for every builtin.
    pub fn is_undeclared_field(&self, field: &str) -> bool {
        !TOOL_INPUT_TRANSPORT_WRAPPER_KEYS.contains(&field)
            && !TOOL_INPUT_IDENTITY_KEYS.contains(&field)
            && !self
                .retired_fields
                .iter()
                .any(|retired| retired.name == field)
            && !self
                .wire_params()
                .any(|param| param.name == field || param.aliases.contains(&field))
    }

    /// The refusal for input carrying undeclared fields, under
    /// [`UnknownFields::Refuse`].
    ///
    /// A suggestion is drawn from the MCP parameters first, in schema order,
    /// then from the other wire parameters, so it names what the caller was
    /// shown.
    fn unknown_field_refusal(&self, object: &Map<String, Value>) -> OrbitError {
        let mut declared = self
            .mcp_params()
            .map(|(param, _)| param.name)
            .collect::<Vec<_>>();
        for param in self.wire_params() {
            for name in std::iter::once(&param.name).chain(param.aliases) {
                if !declared.contains(name) {
                    declared.push(name);
                }
            }
        }
        declared.extend(TOOL_INPUT_IDENTITY_KEYS);
        declared.extend(self.retired_fields.iter().map(|retired| retired.name));
        match reject_unknown_tool_fields(&Value::Object(object.clone()), &declared) {
            Err(error) => error,
            Ok(()) => OrbitError::InvalidInput(format!(
                "{} input carries an undeclared field",
                self.tool_name
            )),
        }
    }

    /// Whether canonical `input` sets none of [`Self::at_least_one_of`].
    /// Always false for an operation that declares no such rule.
    pub fn misses_at_least_one_of(&self, input: &Map<String, Value>) -> bool {
        !self.at_least_one_of.is_empty()
            && !self
                .at_least_one_of
                .iter()
                .any(|field| input.get(*field).is_some_and(|value| !value.is_null()))
    }

    /// Check a call's input against this spec and canonicalize it.
    ///
    /// In order: the input must be a JSON object (`null` reads as `{}`);
    /// declared aliases are renamed to their parameter, refusing input that
    /// sets both; retired fields are refused with their replacement;
    /// undeclared fields are refused under [`UnknownFields::Refuse`], and
    /// otherwise reported on [`OperationInput::unknown_fields`] and left in
    /// place; each present value must match its [`ParamType`],
    /// where an explicit `null` means absent; required parameters must be
    /// present, and so must one of [`Self::at_least_one_of`].
    ///
    /// Only presence and shape are checked. Content rules, such as refusing a
    /// whitespace-only value or an unknown status name, belong to the handler.
    /// An attribution key is never required here: the session may assert it
    /// instead, and the handler refuses a call that resolves none.
    ///
    /// Parsing canonical input again yields the same input, so a host may
    /// re-wrap what the tool entry already parsed.
    pub fn parse_input(&self, input: Value) -> Result<OperationInput, OrbitError> {
        let mut object = match input {
            Value::Object(object) => object,
            Value::Null => Map::new(),
            _ => {
                return Err(OrbitError::InvalidInput(format!(
                    "{} input must be a JSON object",
                    self.tool_name
                )));
            }
        };

        for param in self.wire_params() {
            for alias in param.aliases {
                let Some(value) = object.remove(*alias) else {
                    continue;
                };
                if object.contains_key(param.name) {
                    return Err(OrbitError::InvalidInput(format!(
                        "`{alias}` is an alias of `{}`; set one or the other, not both",
                        param.name
                    )));
                }
                object.insert(param.name.to_string(), value);
            }
        }

        if let Some(retired) = self
            .retired_fields
            .iter()
            .find(|retired| object.contains_key(retired.name))
        {
            return Err(retired.refusal(self.tool_name));
        }

        // Warn mode: the tool entry reports these and the call proceeds.
        // Handlers read declared names only, so the fields have no effect.
        let unknown_fields: Vec<String> = object
            .keys()
            .filter(|field| self.is_undeclared_field(field))
            .cloned()
            .collect();
        if self.unknown_fields == UnknownFields::Refuse && !unknown_fields.is_empty() {
            return Err(self.unknown_field_refusal(&object));
        }

        for param in self.wire_params() {
            match object.get(param.name) {
                None => {}
                Some(Value::Null) => {
                    object.remove(param.name);
                }
                Some(value) => check_param_type(param, value)?,
            }
        }

        let input = Value::Object(object);
        if let Some(missing) = self.wire_params().find(|param| {
            param.required
                && !TOOL_INPUT_IDENTITY_KEYS.contains(&param.name)
                && input.get(param.name).is_none()
        }) {
            return Err(missing_tool_field(&input, missing.name));
        }
        if let Value::Object(object) = &input
            && self.misses_at_least_one_of(object)
        {
            return Err(OrbitError::InvalidInput(format!(
                "{} requires {}",
                self.tool_name,
                alternatives(
                    self.at_least_one_of
                        .iter()
                        .map(|field| format!("`{field}`"))
                )
            )));
        }

        Ok(OperationInput {
            input,
            unknown_fields,
        })
    }
}

/// Join labels as alternatives: `a`, `a or b`, `a, b, or c`.
pub fn alternatives(labels: impl IntoIterator<Item = String>) -> String {
    let labels = labels.into_iter().collect::<Vec<_>>();
    match labels.as_slice() {
        [] => String::new(),
        [only] => only.clone(),
        [first, second] => format!("{first} or {second}"),
        [init @ .., last] => format!("{}, or {last}", init.join(", ")),
    }
}

fn check_param_type(param: &ParamSpec, value: &Value) -> Result<(), OrbitError> {
    let name = param.name;
    let (valid, expected) = match param.param_type {
        ParamType::String | ParamType::Path => (value.is_string(), "a string"),
        ParamType::StringList | ParamType::EnumList(_) => match value {
            Value::Array(items) if !items.iter().all(Value::is_string) => {
                return Err(OrbitError::InvalidInput(format!(
                    "`{name}` entries must be strings"
                )));
            }
            _ => (
                value.is_string() || value.is_array(),
                "a string or array of strings",
            ),
        },
        ParamType::Integer | ParamType::UnsignedInteger => (
            non_negative_integer(value).is_some(),
            "a non-negative integer",
        ),
        ParamType::Boolean | ParamType::Switch => (value.is_boolean(), "a boolean"),
        ParamType::Enum(_) => (value.is_string(), "a string"),
        ParamType::Object => (value.is_object(), "an object"),
        ParamType::Array => (value.is_array(), "an array"),
    };
    if valid {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "`{name}` must be {expected}"
    )))
}
