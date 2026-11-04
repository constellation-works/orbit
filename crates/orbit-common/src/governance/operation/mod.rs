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

mod builders;
mod cli;
mod effects;
mod input;
mod params;
mod registry;
mod spec;

pub(crate) use builders::{
    aliased, both, cli_actions, cli_only, cli_required, defaulted, free_text, mcp_only,
    preserve_empty, required,
};
pub use cli::{CliAction, CliArgKind, CliBinding, CliConstraint, CliGroup, CliHelp};
pub use effects::{Effect, RETIRED_AGENT_FIELD, RetiredField, RetiredWording, UnknownFields};
pub use input::OperationInput;
pub use params::{Description, EnumValue, ParamSpec, ParamType, Redaction};
pub(crate) use registry::operation_registry;
pub use registry::{NounSpec, OperationVerb, ToolOnlyRegistration};
pub use spec::{OperationSpec, alternatives};
