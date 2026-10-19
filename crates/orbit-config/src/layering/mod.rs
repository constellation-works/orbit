//! Layered resolution and source provenance.
//!
//! Reads the global and workspace documents, merges them per key into one
//! document that [`crate::resolved`] admits, and answers "where did this value
//! come from?" for `orbit config show`/`get`.
//!
//! These layering rules live here and nowhere else:
//! - nested tables merge recursively, so a workspace can override one crew
//!   field without restating the crew;
//! - a registry key is one setting, so a workspace value for a registered
//!   table key replaces the global table rather than merging into it;
//! - the replace-only keys below never inherit from global once a distinct
//!   workspace file exists;
//! - the `[machine]` table is global-only: a workspace file that supplies it
//!   is refused before the merge, so a checkout can never rename, renumber, or
//!   re-identify the machine it happens to be checked out on;
//! - a crew name containing `:` is refused per layer, before the merge, so the
//!   error names the file that defines it — the only way back from a persisted
//!   colon-named crew is editing that file;
//! - `[plugin_enablement]` is workspace-only and is lifted off the workspace
//!   file before the merge ([`crate::plugin_enablement`]), so a file holding
//!   only plugin toggles does not count as a distinct workspace file for the
//!   replace-only keys.
//!
//! The review keys are resolved per layer by [`crate::operation`] rather than
//! from the merged document, so their provenance is exact. Each document's
//! deprecated spellings are translated to their replacement keys before the
//! merge ([`crate::aliases`]), the workspace file against the global file
//! beneath it, so the merge, the snapshot and `orbit config get` all see the
//! translated value.

// `load` admits documents, `document` owns TOML helpers, `provenance` attributes
// values, and `types` defines the effective-config view.

mod document;
mod load;
mod provenance;
mod types;

pub(crate) use document::{set_value_at_path, value_at_path};
pub use load::load_effective_config;
pub(crate) use load::{
    LoadedResolvedConfig, load_layered_resolved, reject_workspace_machine_table,
    resolve_workspace_file_document, validate_staged_global_document,
    validate_staged_workspace_document,
};
pub use types::{
    ConfigFallback, ConfigValueSource, ConfigValueSourceKind, ConfigValueState, EffectiveConfig,
    EffectiveConfigValue, ShadowReason, ShadowedConfigValue,
};

/// Security-sensitive settings that a workspace file must restate to keep.
/// Inheriting a machine-global sandbox, approval, or environment allowlist
/// into a workspace that never asked for it is the failure mode this prevents.
const WORKSPACE_REPLACE_ONLY_KEYS: &[&str] = &[
    "execution.codex.approval_policy",
    "execution.codex.sandbox",
    "execution.env.pass",
];

/// The one key an environment variable can supply, below both files.
const DEFAULT_CREW_KEY: &str = "workflow.default_crew";
const DEFAULT_PROVIDER_ENV: &str = "CONSTELLATION_DEFAULT_PROVIDER";
