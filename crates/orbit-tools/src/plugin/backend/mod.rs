//! What every tool of one plugin shares: the backend program, the granted
//! profile the sandbox enforces, the declared programs it may execute, and
//! the child environment (design §4.2, §4.3).
//!
//! The manifest's `permissions` are requests. By the time a [`PluginBackendSpec`]
//! exists the host has checked that every required grant is recorded
//! (`orbit-core`'s plugin host registers the tools inactive otherwise), so
//! the profile resolved here is exactly the granted one.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Child;

use orbit_common::OrbitError;
use orbit_common::security::child_env::{allowlisted_child_env, allowlisted_child_env_from};
use orbit_exec::{ExecRequest, InheritedFd, Sandbox};
use orbit_types::plugin::{
    PLUGIN_HOST_API, PluginGrant, PluginGrantSet, PluginManifestError, PluginNetworkPermission,
    PluginPermissions, PluginProvenance, PluginSandbox, PluginTemplateVars, render_template,
};
use orbit_types::policy::ResolvedFsProfile;
use serde_json::Value;

use super::callback::{PLUGIN_CALLBACK_FD, PluginCallbackSession};
use super::loader::physical_with_missing_tail;
use crate::builtin::proc::spawn::enforce_program_allowlist;
use crate::{TIMEOUT_SLOW_MS, ToolContext, upsert_env};

mod execution;
mod model;
mod programs;
mod sandbox;

pub use self::model::*;
pub use self::programs::*;
pub use self::sandbox::*;
