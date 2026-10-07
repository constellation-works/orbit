//! Operations-as-data kernel shared by Orbit's consumer surfaces.
//!
//! An *operation* is one verb on one noun (`friction add`, `friction list`).
//! This module lets a noun declare each of its verbs exactly once, as data:
//! the wire name, the parameters, how each parameter binds to the CLI, whether
//! MCP exposes the verb, and how the CLI renders the result. Consumer surfaces
//! then *derive* their wiring from that declaration instead of restating the
//! verb once per surface.
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
//! | `orbit-tools`    | `ToolSchema` + optional MCP scope from the spec     |
//! | `orbit-cli`      | `clap::Command` + input JSON + audit metadata       |
//! | `orbit-web`      | tool names + request→input projection               |
//! | `orbit-core`     | the handler table, keyed by the verb enum           |
//!
//! Handlers need `&OrbitRuntime`, which lives above this crate, so the handler
//! table cannot live next to the spec table. The two halves are joined by the
//! noun's verb enum: the spec table is `&'static [OperationSpec<V>]` here, and
//! the handler table is one exhaustive `match` on `V` in `orbit-core`. The
//! compiler rejects a verb that has a spec but no handler.
//!
//! See `docs/design/operations-as-data/` for the migration cookbook.

use orbit_types::tool::McpToolScope;

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
    /// A boolean; on the CLI an explicit `--flag true|false` value.
    Boolean,
}

impl ParamType {
    /// The `ToolParam::param_type` token for this parameter type.
    pub fn as_tool_param_type(self) -> &'static str {
        match self {
            ParamType::String => "string",
            ParamType::StringList => "string_list",
            ParamType::Integer => "integer",
            ParamType::Boolean => "boolean",
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
    },
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

/// One parameter of one operation, in every surface's terms at once.
#[derive(Debug, Clone, Copy)]
pub struct ParamSpec {
    /// The wire field name carried in the operation's JSON input.
    pub name: &'static str,
    /// Wire type.
    pub param_type: ParamType,
    /// Whether the operation rejects input that omits this parameter.
    pub required: bool,
    /// Whether an explicit empty string is meaningful to the handler.
    /// Optional CLI strings are trimmed, then omitted when blank unless this
    /// is set. Only applies to [`ParamType::String`].
    pub allow_empty: bool,
    /// MCP tool-schema description. `None` keeps the parameter off the MCP
    /// schema entirely.
    pub mcp_description: Option<Description>,
    /// Command-line binding. `None` keeps the parameter off the CLI entirely.
    pub cli: Option<CliBinding>,
}

impl ParamSpec {
    /// The `<VALUE_NAME>` clap renders for this parameter.
    ///
    /// Matches what `#[derive(Args)]` produces for a field of the same name, so
    /// migrating a hand-written clap struct to a spec does not move help text.
    pub fn cli_value_name(&self) -> String {
        self.name.to_ascii_uppercase()
    }

    /// The flag spelling, or `None` for positional/CLI-hidden parameters.
    pub fn cli_long(&self) -> Option<&'static str> {
        match self.cli.map(|binding| binding.kind) {
            Some(CliArgKind::Flag { long, .. }) => Some(long),
            _ => None,
        }
    }
}

/// How the CLI renders an operation's response when `--json` is not passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliRender {
    /// Always pretty-print JSON, regardless of `--json`. For aggregate
    /// responses that have no meaningful flat rendering.
    AlwaysJson,
    /// A single record: key/value lines, falling back to JSON for non-objects.
    Record,
    /// An array of records: a table, falling back to JSON for non-arrays.
    RecordTable,
    /// An array of taxonomy entries: one `name[\tdescription]` line each.
    TagList,
}

/// One verb on one noun, declared once.
///
/// `V` is the noun's verb enum. Keeping it typed (rather than a bare string)
/// is what lets `orbit-core`'s handler table be exhaustiveness-checked against
/// this registry.
#[derive(Debug, Clone, Copy)]
pub struct OperationSpec<V: 'static> {
    /// The typed verb this spec describes.
    pub verb: V,
    /// The verb's short name: the CLI subcommand and the audit `subcommand`.
    pub name: &'static str,
    /// The fully-qualified tool name (`orbit.friction.add`).
    pub tool_name: &'static str,
    /// MCP tool description.
    pub tool_description: &'static str,
    /// `--help` summary for the CLI subcommand.
    pub cli_about: &'static str,
    /// Parameters, in declaration order. Order is contract: it drives both MCP
    /// schema order and CLI `--help` order.
    pub params: &'static [ParamSpec],
    /// Whether the operation rejects a legacy top-level `agent` field
    /// (attribution was consolidated to `model`-only).
    pub rejects_agent_field: bool,
    /// MCP scope when advertised. `None` keeps the operation off MCP while it
    /// remains reachable through its other registered surfaces.
    ///
    /// This is placement, not permission: it decides who is *shown* the verb,
    /// never who may perform it. Authorization is
    /// [`crate::governance::authorization::GOVERNED_OPERATIONS`] alone — see that module's
    /// "Placement is not permission" section.
    pub mcp_scope: Option<McpToolScope>,
    /// Whether the CLI subcommand offers `--json`.
    pub cli_json_flag: bool,
    /// Default (non-`--json`) CLI rendering.
    pub cli_render: CliRender,
}

impl<V: 'static> OperationSpec<V> {
    /// Parameters that MCP advertises, in declaration order.
    pub fn mcp_params(&self) -> impl Iterator<Item = (&'static ParamSpec, Description)> {
        self.params.iter().filter_map(|param| {
            param
                .mcp_description
                .map(|description| (param, description))
        })
    }

    /// Parameters bound to the command line, in declaration order.
    pub fn cli_params(&self) -> impl Iterator<Item = (&'static ParamSpec, CliBinding)> {
        self.params
            .iter()
            .filter_map(|param| param.cli.map(|binding| (param, binding)))
    }

    /// Whether the CLI takes a positional `id`, which is also the audit target.
    pub fn cli_positional(&self) -> Option<&'static ParamSpec> {
        self.params.iter().find(|param| {
            matches!(
                param.cli.map(|binding| binding.kind),
                Some(CliArgKind::Positional)
            )
        })
    }
}

/// Look up a spec by its short verb name.
pub fn find_by_name<'a, V: 'static>(
    registry: &'a [OperationSpec<V>],
    name: &str,
) -> Option<&'a OperationSpec<V>> {
    registry.iter().find(|spec| spec.name == name)
}
