//! Operations-as-data kernel shared by Orbit's consumer surfaces.
//!
//! An *operation* is one verb on one noun (`friction add`, `friction list`).
//! This module lets a noun declare each of its verbs exactly once, as data:
//! the wire name, whether it reads or writes, the parameters, how each
//! parameter binds to the CLI and whether it is redacted, and whether MCP
//! exposes the verb. Consumer surfaces then *derive* their wiring from that
//! declaration instead of restating the verb once per surface.
//!
//! A fact only one surface needs, such as how the CLI renders a response, is
//! not part of the spec: it is an exhaustive `match` on the verb enum in that
//! surface's crate, so adding a verb fails to compile there until it is
//! decided.
//!
//! # Layering
//!
//! The kernel is deliberately transport-agnostic and runtime-agnostic: it holds
//! no clap types, no axum types, and no `OrbitRuntime` handle, so it can live in
//! the leaf crate where every surface can read it. Each surface owns its own
//! translation:
//!
//! | Surface          | Derives                                            |
//! |------------------|----------------------------------------------------|
//! | `orbit-tools`    | `ToolSchema`, MCP scope and execution kind          |
//! | `orbit-cli`      | `clap::Command`, input JSON, audit and runtime need |
//! | `orbit-web`      | tool names + request→input projection               |
//! | `orbit-core`     | the handler table and redaction, keyed by the verb  |
//!
//! Handlers need `&OrbitRuntime`, which lives above this crate, so the handler
//! table cannot live next to the spec table. The two halves are joined by the
//! noun's verb enum: the spec table is `&'static [OperationSpec<V>]` here, and
//! the handler table is one exhaustive `match` on `V` in `orbit-core`. The
//! compiler rejects a verb that has a spec but no handler.
//!
//! # Input
//!
//! [`OperationSpec::parse_input`] is the one place a call's input is checked
//! against its spec. The generic operation tool runs it before dispatch, so
//! every surface that reaches a tool gets the same validation, and handlers
//! read canonical parameter names from the [`OperationInput`] it returns.
//!
//! See `docs/design/operations-as-data/` for the migration cookbook.

use orbit_types::tool::McpToolScope;
use serde_json::{Map, Value};

use crate::OrbitError;
use crate::protocol::tool_input::{
    TOOL_INPUT_IDENTITY_KEYS, TOOL_INPUT_TRANSPORT_WRAPPER_KEYS, missing_tool_field,
    optional_csv_or_string_list_alias, optional_raw_string, optional_string, required_string,
};

/// Wire type of an operation parameter.
///
/// The string forms are the `ToolParam::param_type` vocabulary consumed by the
/// MCP JSON-Schema composer; they are contract, not cosmetics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    /// A single string value.
    String,
    /// A string, or an array of strings; comma-separated strings are accepted.
    StringList,
    /// A non-negative integer.
    Integer,
    /// A non-negative integer with the historical `u64` tool-schema token.
    UnsignedInteger,
    /// A boolean; on the CLI an explicit `--flag true|false` value.
    Boolean,
    /// A boolean that is a presence flag on the CLI: `--flag` sends `true`,
    /// and omitting it sends nothing.
    Switch,
    /// One of a fixed set of string values. The CLI offers them as possible
    /// values; the wire carries the string. The tool entry checks only that a
    /// string arrived: membership is a content rule the handler owns.
    Enum(&'static [EnumValue]),
    /// A JSON object, for structured input such as a schedule.
    Object,
}

/// One allowed value of a [`ParamType::Enum`] parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnumValue {
    /// The value as the CLI advertises and sends it.
    pub name: &'static str,
    /// Per-value `--help` text. A value list where any value has help is
    /// rendered one value per line.
    pub help: Option<&'static str>,
    /// Other spellings the CLI accepts for this value, hidden from `--help`.
    pub aliases: &'static [&'static str],
}

impl EnumValue {
    /// A value with no help text and no aliases.
    pub const fn plain(name: &'static str) -> Self {
        Self {
            name,
            help: None,
            aliases: &[],
        }
    }

    /// A value with per-value help text.
    pub const fn helped(name: &'static str, help: &'static str) -> Self {
        Self {
            name,
            help: Some(help),
            aliases: &[],
        }
    }
}

impl ParamType {
    /// The `ToolParam::param_type` token for this parameter type.
    pub fn as_tool_param_type(self) -> &'static str {
        match self {
            ParamType::String => "string",
            ParamType::StringList => "string_list",
            ParamType::Integer => "integer",
            ParamType::UnsignedInteger => "u64",
            ParamType::Boolean | ParamType::Switch => "boolean",
            // Allowed values stay in the parameter's description, so a
            // migrated enum is wire-identical to the string it replaced.
            ParamType::Enum(_) => "string",
            ParamType::Object => "object",
        }
    }
}

/// Help/description text that is either fixed or assembled at call time.
///
/// Some descriptions interpolate live configuration (the friction taxonomy, for
/// example), so the spec stores a resolver rather than a literal. Both variants
/// are const-constructible, which keeps whole registries as `const` items.
#[derive(Debug, Clone, Copy)]
pub enum Description {
    /// A fixed literal.
    Static(&'static str),
    /// Computed on demand, for text that interpolates live data.
    Computed(fn() -> String),
}

impl Description {
    /// Materialize the description text.
    pub fn resolve(&self) -> String {
        match self {
            Description::Static(text) => (*text).to_string(),
            Description::Computed(resolve) => resolve(),
        }
    }
}

/// How a parameter appears on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliArgKind {
    /// A required positional argument.
    Positional,
    /// A `--long` flag taking one value.
    Flag {
        /// The flag spelling, without leading dashes. This is intentionally
        /// separate from the parameter name: `friction add` takes `--tag` but
        /// sends `tags` over the wire.
        long: &'static str,
        /// When set, one occurrence may carry several delimiter-separated
        /// values (`--tag a,b`). Only meaningful for [`ParamType::StringList`].
        delimiter: Option<char>,
        /// Other flag spellings accepted and hidden from `--help`.
        aliases: &'static [&'static str],
        /// Other flag spellings accepted and listed by `--help`.
        visible_aliases: &'static [&'static str],
        /// The `<VALUE_NAME>` `--help` shows, when it is not the parameter name
        /// in SCREAMING_SNAKE.
        value_name: Option<&'static str>,
    },
}

impl CliArgKind {
    /// A `--<long>` flag with no aliases, delimiter or value-name override.
    pub const fn flag(long: &'static str) -> Self {
        CliArgKind::Flag {
            long,
            delimiter: None,
            aliases: &[],
            visible_aliases: &[],
            value_name: None,
        }
    }
}

/// A parameter's command-line binding: how it is spelled and how it is helped.
#[derive(Debug, Clone, Copy)]
pub struct CliBinding {
    /// Positional or flag.
    pub kind: CliArgKind,
    /// Help text shown by `--help`. Frequently differs from the MCP description
    /// because the audiences differ; both are contract once shipped.
    pub help: Description,
}

/// How a parameter's value is redacted before an operation persists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redaction {
    /// Stored as given: identifiers, enumerated values, structural input.
    None,
    /// Free text: secrets and credentials are masked, a whole-token credential
    /// is refused, and home-directory paths are shortened.
    FreeText,
    /// A filesystem path or URL: only home-directory paths are shortened.
    Path,
}

/// One parameter of one operation, in every surface's terms at once.
#[derive(Debug, Clone, Copy)]
pub struct ParamSpec {
    /// The wire field name carried in the operation's JSON input.
    pub name: &'static str,
    /// Other wire spellings the operation accepts for this parameter.
    /// [`OperationSpec::parse_input`] renames each to `name`, so handlers and
    /// redaction see only the canonical name, and refuses input that sets
    /// both. Aliases appear on no schema and no CLI flag.
    pub aliases: &'static [&'static str],
    /// Wire type.
    pub param_type: ParamType,
    /// Whether the operation rejects input that omits this parameter.
    pub required: bool,
    /// Whether an explicitly supplied empty string has meaning, such as
    /// clearing a stored value. The CLI preserves trimmed empty values for
    /// optional [`ParamType::String`] parameters when true; otherwise it omits
    /// them. Required strings and other parameter types are unaffected.
    pub preserve_empty: bool,
    /// The value the CLI sends when the flag is omitted, shown by `--help` as
    /// `[default: …]`. The MCP schema states a default in the description,
    /// and the tool entry inserts none: the handler's own default applies to
    /// a call that omits the field.
    pub default: Option<&'static str>,
    /// How the tool host redacts this parameter before it is persisted.
    pub redact: Redaction,
    /// MCP tool-schema description. `None` keeps the parameter off the MCP
    /// schema entirely.
    pub mcp_description: Option<Description>,
    /// Command-line binding. `None` keeps the parameter off the CLI entirely.
    ///
    /// A parameter with a binding and no MCP description is *CLI-only*: the
    /// noun's CLI surface table consumes it while shaping the request (several
    /// flags composing one structured field, or a client-side filter), and it
    /// is not a wire field, so the tool entry treats it as undeclared.
    pub cli: Option<CliBinding>,
}

impl ParamSpec {
    /// The `<VALUE_NAME>` clap renders for this parameter.
    ///
    /// Matches what `#[derive(Args)]` produces for a field of the same name, so
    /// migrating a hand-written clap struct to a spec does not move help text.
    pub fn cli_value_name(&self) -> String {
        match self.cli.map(|binding| binding.kind) {
            Some(CliArgKind::Flag {
                value_name: Some(value_name),
                ..
            }) => value_name.to_string(),
            _ => self.name.to_ascii_uppercase(),
        }
    }

    /// Whether this parameter exists only on the CLI and never reaches the
    /// tool; see [`Self::cli`].
    pub fn is_cli_only(&self) -> bool {
        self.mcp_description.is_none() && self.cli.is_some()
    }
}

/// Whether an operation reads or writes durable state.
///
/// One declaration that every surface derives its runtime wiring from: the
/// tool's execution kind and the CLI's default runtime need. A guard that
/// depends on the input, such as a write permission needed only when an update
/// moves a record, stays in the handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Observes state without changing it.
    Read,
    /// May change durable state.
    Write,
}

/// A field an operation used to accept and now refuses.
#[derive(Debug, Clone, Copy)]
pub struct RetiredField {
    /// The retired wire field name.
    pub name: &'static str,
    /// What to send instead; the refusal ends with it.
    pub replacement: &'static str,
}

impl RetiredField {
    fn refusal(&self, tool_name: &str) -> OrbitError {
        OrbitError::InvalidInput(format!(
            "{tool_name} no longer accepts `{}`; {}",
            self.name, self.replacement
        ))
    }
}

/// The legacy top-level `agent` field: attribution was consolidated to
/// `model`-only.
pub const RETIRED_AGENT_FIELD: RetiredField = RetiredField {
    name: "agent",
    replacement: "use `model` with the agent family for attribution",
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
    /// Whether the operation reads or writes.
    pub effect: Effect,
    /// Parameter groups, concatenated in order. Order is contract: it drives
    /// both MCP schema order and CLI `--help` order. A group lets verbs share a
    /// run of parameters without functional record update, which `const` does
    /// not allow.
    pub params: &'static [&'static [ParamSpec]],
    /// Parameters of which a call must set at least one, such as the edits an
    /// `update` applies. Empty when the operation has no such rule.
    pub at_least_one_of: &'static [&'static str],
    /// Fields the operation used to accept and now refuses, naming where each
    /// went.
    pub retired_fields: &'static [RetiredField],
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

    /// Whether the CLI takes a positional `id`, which is also the audit target.
    pub fn cli_positional(&self) -> Option<&'static ParamSpec> {
        self.params().find(|param| {
            matches!(
                param.cli.map(|binding| binding.kind),
                Some(CliArgKind::Positional)
            )
        })
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
    /// undeclared fields are reported on [`OperationInput::unknown_fields`]
    /// and left in place; each present value must match its [`ParamType`],
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
        let unknown_fields = object
            .keys()
            .filter(|field| self.is_undeclared_field(field))
            .cloned()
            .collect();

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
        ParamType::String => (value.is_string(), "a string"),
        ParamType::StringList => match value {
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
    };
    if valid {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "`{name}` must be {expected}"
    )))
}

/// An integer parameter's value: a JSON number or a numeric string.
fn non_negative_integer(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(raw) => raw.trim().parse().ok(),
        _ => None,
    }
}

/// A call's input after [`OperationSpec::parse_input`]: a JSON object keyed by
/// canonical parameter names, whose present values match their declared types.
///
/// The accessors apply the content rules every handler shared before the
/// kernel checked input: trimmed strings, non-empty unless read raw, and the
/// lenient string-list forms.
#[derive(Debug, Clone)]
pub struct OperationInput {
    input: Value,
    unknown_fields: Vec<String>,
}

impl OperationInput {
    /// Undeclared fields the input carried, in input order. They are reported
    /// by the tool entry and ignored by handlers.
    pub fn unknown_fields(&self) -> &[String] {
        &self.unknown_fields
    }

    /// The canonical input object.
    pub fn as_value(&self) -> &Value {
        &self.input
    }

    /// The canonical input object, for a host that takes JSON.
    pub fn into_value(self) -> Value {
        self.input
    }

    /// A trimmed, non-empty string the operation requires.
    pub fn required_string(&self, name: &str) -> Result<String, OrbitError> {
        required_string(&self.input, &[name], name)
    }

    /// A trimmed string; present but blank is refused.
    pub fn optional_string(&self, name: &str) -> Result<Option<String>, OrbitError> {
        optional_string(&self.input, name)
    }

    /// A string exactly as sent, for parameters where blank has meaning.
    pub fn optional_raw_string(&self, name: &str) -> Result<Option<String>, OrbitError> {
        optional_raw_string(&self.input, name)
    }

    /// A string list from an array, a string, a comma-separated string or a
    /// JSON-encoded array string.
    pub fn optional_string_list(&self, name: &str) -> Result<Option<Vec<String>>, OrbitError> {
        optional_csv_or_string_list_alias(&self.input, &[name])
    }

    /// A non-negative integer from a number or a numeric string.
    pub fn optional_count(&self, name: &str) -> Result<Option<usize>, OrbitError> {
        let Some(value) = self.input.get(name) else {
            return Ok(None);
        };
        non_negative_integer(value)
            .and_then(|count| usize::try_from(count).ok())
            .map(Some)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!("`{name}` must be a non-negative integer"))
            })
    }

    /// A boolean.
    pub fn optional_bool(&self, name: &str) -> Result<Option<bool>, OrbitError> {
        match self.input.get(name) {
            None => Ok(None),
            Some(Value::Bool(value)) => Ok(Some(*value)),
            Some(_) => Err(OrbitError::InvalidInput(format!(
                "`{name}` must be a boolean"
            ))),
        }
    }
}

/// An intermediate CLI subcommand that groups verbs (`task artifact`).
///
/// Declared for nouns whose verbs nest; the CLI adapter builds groups once the
/// first such noun migrates. Friction declares none.
#[derive(Debug, Clone, Copy)]
pub struct CliGroup {
    /// The group's subcommand name, matching the first `cli_path` segment of
    /// the verbs it holds.
    pub name: &'static str,
    /// `--help` summary for the group.
    pub cli_about: &'static str,
}

/// Registration of a noun's tools that have no MCP scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOnlyRegistration {
    /// Advertise the tool on the active local tool surface, without MCP.
    Active,
    /// Keep the tool reachable but hidden from the active and MCP surfaces.
    Inactive,
}

/// One noun: the strings every surface needs about it, and its operations.
#[derive(Debug, Clone, Copy)]
pub struct NounSpec<V: 'static> {
    /// The CLI command (`friction`, `auto-task`).
    pub cli_name: &'static str,
    /// The tool-name prefix (`orbit.friction`, `orbit.auto_task`). Separate
    /// from `cli_name` because shipped surfaces already spell them differently.
    pub tool_prefix: &'static str,
    /// Registration for operations whose `mcp_scope` is `None`.
    pub tool_only_registration: ToolOnlyRegistration,
    /// `orbit <noun> --help` summary.
    pub cli_about: &'static str,
    /// The audit `target_type` the noun's CLI rows carry.
    pub audit_target_type: &'static str,
    /// The parameter whose value is the audit `target_id`, in every verb that
    /// declares it (`id`, `name`).
    pub audit_target_param: &'static str,
    /// Intermediate CLI subcommands, in `--help` order.
    pub cli_groups: &'static [CliGroup],
    /// Every operation, in `--help` order.
    pub operations: &'static [OperationSpec<V>],
}

/// A noun's verb enum, as the generic surface adapters see it.
///
/// Implemented by the crate-internal `operation_registry!` macro, which emits
/// the enum, this impl and the ordered operations list from one declaration.
pub trait OperationVerb: Copy + Eq + std::fmt::Debug + Send + Sync + 'static {
    /// The noun this verb belongs to.
    fn noun() -> &'static NounSpec<Self>;

    /// This verb's specification.
    fn spec(self) -> &'static OperationSpec<Self>;

    /// The verb's fully-qualified tool name (`orbit.friction.add`).
    fn tool_name(self) -> &'static str {
        self.spec().tool_name
    }
}

/// Declare a noun's verb enum, its [`OperationVerb`] impl and its ordered
/// operations list from one list of `Variant => SPEC` entries.
///
/// The three cannot disagree: list order is both enum order and `--help`
/// order, and `spec()` is an exhaustive match generated from the same entries.
macro_rules! operation_registry {
    (
        noun: $noun:ident;
        $(#[$enum_meta:meta])*
        $enum_vis:vis enum $verb:ident;
        $(#[$list_meta:meta])*
        $list_vis:vis const $list:ident;
        {
            $( $(#[$variant_meta:meta])* $variant:ident => $spec:ident ),+ $(,)?
        }
    ) => {
        $(#[$enum_meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        $enum_vis enum $verb {
            $( $(#[$variant_meta])* $variant, )+
        }

        impl $crate::governance::operation::OperationVerb for $verb {
            fn noun() -> &'static $crate::governance::operation::NounSpec<Self> {
                &$noun
            }

            fn spec(self) -> &'static $crate::governance::operation::OperationSpec<Self> {
                match self {
                    $( $verb::$variant => &$spec, )+
                }
            }
        }

        $(#[$list_meta])*
        $list_vis const $list: &[$crate::governance::operation::OperationSpec<$verb>] =
            &[ $( $spec ),+ ];
    };
}

pub(crate) use operation_registry;
