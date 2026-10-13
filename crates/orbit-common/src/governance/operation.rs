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
    /// Wire type.
    pub param_type: ParamType,
    /// Whether the operation rejects input that omits this parameter.
    pub required: bool,
    /// Whether an explicitly supplied empty string has meaning, such as
    /// clearing a stored value. The CLI preserves trimmed empty values for
    /// optional [`ParamType::String`] parameters when true; otherwise it omits
    /// them. Required strings and other parameter types are unaffected.
    pub preserve_empty: bool,
    /// How the tool host redacts this parameter before it is persisted.
    pub redact: Redaction,
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
}

impl<V: 'static> OperationSpec<V> {
    /// Every parameter, in declaration order across its groups.
    pub fn params(&self) -> impl Iterator<Item = &'static ParamSpec> {
        self.params.iter().flat_map(|group| group.iter())
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

/// One noun: the strings every surface needs about it, and its operations.
#[derive(Debug, Clone, Copy)]
pub struct NounSpec<V: 'static> {
    /// The CLI command (`friction`, `auto-task`).
    pub cli_name: &'static str,
    /// The tool-name prefix (`orbit.friction`, `orbit.auto_task`). Separate
    /// from `cli_name` because shipped surfaces already spell them differently.
    pub tool_prefix: &'static str,
    /// `orbit <noun> --help` summary.
    pub cli_about: &'static str,
    /// The audit `target_type` the noun's CLI rows carry.
    pub audit_target_type: &'static str,
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
