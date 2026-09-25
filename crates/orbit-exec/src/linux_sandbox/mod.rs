//! Linux Bubblewrap write-confinement for CLI-backed agents.
//!
//! The backend deliberately keeps the host filesystem readable and materializes
//! `ResolvedFsProfile::modify` as ordered bind mounts. It is therefore honest
//! write confinement, not a general read-policy implementation.

mod argv;
mod mounts;
mod probe;
mod rules;
mod spawn;
mod types;
mod write_grants;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, OnceLock};

use orbit_common::OrbitError;
use orbit_types::policy::{ResolvedFsProfile, compile_glob_regex};
use regex::Regex;

pub use argv::{compile_linux_bwrap_argv, compile_linux_bwrap_argv_with_authority};
pub use probe::{bwrap_path, bwrap_program_for_audit, bwrap_unavailable_message, probe_bwrap};
pub use spawn::spawn_under_linux_bwrap;
pub use types::{
    BwrapProbeOutcome, LINUX_STABLE_BUILD_MOUNT, LINUX_STABLE_WORKSPACE_MOUNT,
    LinuxBwrapMountAuthority, LinuxBwrapMountEvidence, LinuxBwrapPlan, LinuxBwrapPostRunGuard,
    LinuxBwrapSpawnRequest,
};
pub use write_grants::{
    PreparedWriteGrants, UnsatisfiedWriteGrant, WriteAnchorKind, WriteGrant,
    linux_bwrap_write_grant_diagnostic, linux_bwrap_write_grants, prepare_linux_bwrap_write_grants,
};

use argv::base_namespace_args;
use mounts::{
    append_cargo_download_cache_mounts, append_stable_toolchain_mounts, cargo_home_dir,
    cwd_is_writable_root, profile_grants_write, push_mount,
};
use probe::TRUSTED_BWRAP_PATH;
#[cfg(test)]
use probe::{BwrapProbeMemo, probe_bwrap_with};
#[cfg(test)]
use rules::walk_paths;
use rules::{
    GlobMatches, canonical_existing, exact_or_subtree_root, expand_each_rule, expand_rules,
    is_exact_or_subtree, is_narrow_reallow, mount_paths_for_rule, overlaps_writable_root,
    positive_mount_roots, post_run_deny_rules,
};
#[cfg(test)]
use spawn::inherit_mount_sources;
use spawn::prepare_mount_source;
use write_grants::{CompiledModifyRules, compile_rule_regex, render_glob_path};

#[cfg(test)]
mod tests;
