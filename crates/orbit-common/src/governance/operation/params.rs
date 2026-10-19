#[cfg(doc)]
use super::OperationSpec;
use super::{CliArgKind, CliBinding};

/// Wire type of an operation parameter.
///
/// The string forms are the `ToolParam::param_type` vocabulary consumed by the
/// MCP JSON-Schema composer; they are contract, not cosmetics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    /// A single string value.
    String,
    /// A path string, preserved byte-for-byte by the CLI argument projection.
    Path,
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
    /// Several values of one fixed set. Carried as a [`Self::StringList`]; on
    /// the CLI each value is offered and checked as for [`Self::Enum`].
    EnumList(&'static [EnumValue]),
    /// A JSON object, for structured input such as a schedule.
    Object,
    /// A JSON array of structured entries, such as typed relations. The
    /// handler owns the entries' shape.
    Array,
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
            ParamType::String | ParamType::Path => "string",
            ParamType::StringList | ParamType::EnumList(_) => "string_list",
            ParamType::Integer => "integer",
            ParamType::UnsignedInteger => "u64",
            ParamType::Boolean | ParamType::Switch => "boolean",
            // Allowed values stay in the parameter's description, so a
            // migrated enum is wire-identical to the string it replaced.
            ParamType::Enum(_) => "string",
            ParamType::Object => "object",
            ParamType::Array => "array",
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
