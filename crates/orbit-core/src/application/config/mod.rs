//! Structured `config.toml` views and edits for non-CLI surfaces [ORB-12724].
//!
//! `orbit config show` renders the layered view as text; the dashboard needs
//! the same facts as data. Everything here is a projection of what
//! [`orbit_config`] already resolved — the layering, the three-way value
//! state, the shadowed lower layers, and the registry's section/description/
//! choice metadata. Provenance is never re-derived from the raw documents,
//! so the dashboard and the CLI cannot disagree about where a value came
//! from.
//!
//! Writes go through the same [`orbit_config::ConfigStore`] admission path as
//! `orbit config set`, so a refused value is refused identically here and the
//! caller can render the admission error verbatim.

mod paths;
mod provenance;
mod rows;
mod view;
mod write;

/// Which physical `config.toml` a view or a write targets. Re-exported so a
/// transport can name the scope without depending on `orbit-config` directly.
pub use orbit_config::ConfigScope;

pub use paths::path_rows;
pub use view::{effective_view, file_view, key_catalog, parse_config_scope};
pub use write::{ConfigWriteInit, ConfigWriteOutcome, delete_crew, set_crew, set_key, unset_key};
