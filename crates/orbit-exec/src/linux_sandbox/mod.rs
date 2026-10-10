//! Linux Bubblewrap write-confinement for CLI-backed agents.
//!
//! The backend deliberately keeps the host filesystem readable and materializes
//! `ResolvedFsProfile::modify` as ordered bind mounts. It is therefore honest
//! write confinement, not a general read-policy implementation. It has two
//! read boundaries, both masks emitted after every other mount: the fixed
//! well-known credential locations (`~/.ssh`, `~/.aws`, `~/.config/gh`, cargo
//! publish tokens, ...) that the macOS profile also denies, shared through
//! [`crate::credential_paths`], and an explicit caller-named mask of
//! directories hidden behind a read-only stand-in (see [`LinuxBwrapMask`]).
//! Every other host read stays delegated.

mod argv;
mod credentials;
mod git;
mod mask;
mod mounts;
mod probe;
mod rules;
mod spawn;
mod types;
mod wrapper;
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
#[cfg(target_os = "linux")]
pub use probe::probe_bwrap_fresh_for_user;
pub use probe::{
    bwrap_deferral_notice, bwrap_path, bwrap_program_for_audit, probe_bwrap, probe_bwrap_fresh,
    report_bwrap_deferral,
};
pub use spawn::spawn_under_linux_bwrap;
pub use types::{
    BwrapProbeOutcome, LINUX_STABLE_BUILD_MOUNT, LINUX_STABLE_WORKSPACE_MOUNT, LinuxBwrapMask,
    LinuxBwrapMountAuthority, LinuxBwrapMountEvidence, LinuxBwrapPlan, LinuxBwrapPostRunGuard,
    LinuxBwrapScratchRemoval, LinuxBwrapSpawnRequest,
};
pub use wrapper::{BUNDLED_BWRAP_PATH, BUNDLED_BWRAP_VERSION, BwrapSource, HOST_BWRAP_PATH};
pub use write_grants::{
    UnsatisfiedWriteGrant, WriteAnchorKind, linux_bwrap_write_grant_diagnostic,
    linux_bwrap_write_grants, prepare_linux_bwrap_write_grants,
};

/// Every existing path matched by any of `rules`: absolute globs in the
/// profile grammar, resolved canonically from one walk per search root.
///
/// A kernel ruleset binds inodes, so a caller compiling one from glob rules
/// needs the paths those rules name today: the plugin backend's boundary
/// ([`crate::linux_landlock::LandlockBoundary::read_exclusions`]) and the
/// write roots a brokered backend may keep.
pub fn existing_glob_matches(rules: &[String]) -> Result<BTreeSet<PathBuf>, OrbitError> {
    expand_rules(rules)
}

use crate::credential_paths::CredentialReadDeny;
use argv::base_namespace_args;
use credentials::{append_credential_masks, host_credential_denies, host_mounts};
use git::append_git_metadata_mounts;
#[cfg(test)]
use mask::host_alias;
use mask::{MountEntry, append_mask_mounts};
use mounts::{
    append_cargo_download_cache_mounts, append_stable_toolchain_mounts, cargo_home_dir,
    cwd_is_writable_root, profile_grants_write, push_mount,
};
use rules::{
    GlobMatches, canonical_existing, exact_or_subtree_root, expand_each_rule, expand_rules,
    is_exact_or_subtree, is_narrow_reallow, mount_paths_for_rule, overlaps_writable_root,
    positive_mount_roots, post_run_deny_rules,
};
#[cfg(all(test, target_os = "linux"))]
use spawn::inherit_mount_sources;
use spawn::prepare_mount_source;
use wrapper::trusted_wrapper;
use write_grants::{CompiledModifyRules, compile_rule_regex, render_glob_path};

#[cfg(test)]
mod tests;
