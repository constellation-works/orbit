//! Read and validate one plugin directory.
//!
//! Fail closed per plugin (§4.9): every problem here is reported as a
//! [`PluginLoadError`] naming the manifest field, and the caller decides
//! whether that refuses `orbit plugin add` or registers the plugin inactive.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
// Re-exported: `orbit-core` holds recorded plugin paths to the same physical
// resolution this module's guards use, and reaches it through this crate.
pub use orbit_exec::physical_with_missing_tail;
use orbit_types::plugin::{
    FIRST_PARTY_PUBLISHER, MANIFEST_FILE_NAME, PluginExecutionKind, PluginManifest,
    PluginManifestError, PluginMcpScope, PluginTestFile, RESERVED_CLI_COMMANDS,
    namespace_collides_with_tool, plugin_tool_name,
};
use orbit_types::tool::ToolParam;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use super::backend::{PluginBackendSpec, render_fs_roots};
use super::schema::{CompiledSchema, params_from_input_schema};
use crate::ToolRegistry;

mod load;
mod model;
mod validation;

pub use self::load::*;
pub use self::model::*;
pub use self::validation::*;
