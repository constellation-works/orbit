//! The friction CLI, derived from the friction operation registry.
//!
//! ADR-0209 bearing 1 pilot [ORB-10358]. Each friction verb used to be a clap
//! `Args` struct plus an `Execute` impl that hand-built the tool input — the
//! same names, help strings, and field mapping the MCP tool file and the
//! dashboard handler each restated. All of that is now declared once in
//! `orbit_common::governance::friction::operations`, and the clap and input derivation is the
//! noun-agnostic adapter in [`super::operation_args`].
//!
//! What is left here is genuinely friction-specific and CLI-only: the
//! request shaping in `prepare` and the text rendering in `render`. Both
//! are exhaustive matches on `FrictionVerb`, so a new verb fails to compile
//! here until its CLI behaviour is decided. Field presence rules live in the
//! spec; the adapter only words their refusal in flags.

mod add;
mod command;
mod list;
mod rehome;
mod resolve;
mod show;
mod stats;
mod support;
mod tags;
mod update;

pub use command::FrictionCommand;
