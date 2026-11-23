//! Admitted config documents below layering: dotted-path access and the typed
//! read of one path (`paths`), no-follow reads and path admission for
//! `config.toml` (`io`), and the parse of one admitted file into a TOML
//! document (`parse`).
//!
//! `io` stays free of TOML and of sibling modules; `paths` uses only std, TOML,
//! serde and `orbit-common`. `scripts/check-dependency-direction.sh` enforces
//! those two leaf rules. `parse` composes the two and admits no policy; that is
//! design intent the script does not check.

pub(super) mod io;
pub(super) mod parse;
pub(super) mod paths;

pub use paths::invalid_runtime_config;
