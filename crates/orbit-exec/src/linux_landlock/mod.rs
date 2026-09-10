//! Linux Landlock read confinement for activity-scoped `proc.spawn`.
//!
//! Guessing which argv strings look like paths cannot see what an allowed
//! program will do with them. `git`, `bash`, and `python3` are all on shipped
//! activity allowlists, and every one of them will interpret text supplied on
//! its command line — `git -c alias.x='!cat /etc/shadow' x` reaches the host
//! without a single path-shaped argument. The read boundary therefore has to
//! live where the child does, not in the request.
//!
//! This module compiles the activity's resolved read profile into a Landlock
//! ruleset and applies it between `fork` and `exec`, so the child and every
//! descendant it spawns inherit it. The request-time argv check remains, but
//! only as an early, explainable deny for `git -C /etc`; it is no longer the
//! security boundary.
//!
//! # What the ruleset covers
//! Read and execute access (`READ_FILE`, `READ_DIR`, `EXECUTE`) plus
//! `REFER`, which governs moving a file between directories. Writes are not
//! handled here: agent write confinement belongs to the Bubblewrap mount
//! namespace in [`crate::linux_sandbox`], and handling writes in two places
//! would leave two answers to one question.
//!
//! `REFER` is handled because Landlock refuses a rename or link that would
//! give a file *more* access at its destination. Without it, a child could
//! move a denied file into a fully readable sibling directory and read it
//! there. See [`workspace`] for how denied paths are carved out.
//!
//! # What the ruleset does not cover
//! Landlock rules bind to inodes, so the ruleset cannot single out a *name*
//! that does not exist yet. The compiler answers that by asking what the
//! profile's exclusions can name rather than what they currently match: a
//! directory a bounded exclusion reaches into is granted list-only, so a name
//! created there afterwards has no readable ancestor. An exclusion whose reach
//! crosses directories (`**/.env`) reaches every directory in the workspace,
//! and carving that out would leave the run unable to read the files it
//! produces itself. Those rules are reported on
//! [`LandlockReadBoundary::unenforced_exclusions`] instead of being enforced
//! or quietly dropped. [`workspace`] carries the full reasoning.
//!
//! The boundary also governs acquisition rather than naming: an inode the
//! ruleset already grants keeps that grant through a rename or hard link into
//! a denied name, and bytes already read, mapped, or held on an open
//! descriptor cannot be withdrawn by any later rule.

mod host;
mod workspace;

#[cfg(target_os = "linux")]
mod ruleset;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Child;

use orbit_common::OrbitError;
use orbit_common::tracing;
use orbit_types::policy::ResolvedFsProfile;

use crate::runner::{EnvironmentMode, ExecRequest};

pub use host::HOST_READ_ENV_VARS;

/// Lowest Landlock ABI this enforcement accepts.
///
/// ABI 2 (Linux 5.19) is the first to expose `LANDLOCK_ACCESS_FS_REFER`.
/// Below it a confined child could relocate a denied file into a readable
/// directory, so an older kernel is reported as unavailable rather than
/// enforced with a known hole.
pub const MINIMUM_LANDLOCK_ABI: i64 = 2;

/// What a compiled grant lets the child do with one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LandlockGrant {
    /// Read, execute, and relocate anything beneath a directory.
    ReadTree,
    /// List a directory without reading the files directly inside it.
    ///
    /// Applied to a directory that holds a denied path: its allowed children
    /// are granted individually, so the denied one keeps no readable
    /// ancestor while the rest of the tree stays usable.
    ListOnly,
    /// Read and execute one file.
    ReadFile,
}

/// One compiled Landlock rule: a path and what the child may do beneath it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandlockPathGrant {
    pub path: PathBuf,
    pub grant: LandlockGrant,
}

impl LandlockPathGrant {
    fn read_tree(path: PathBuf) -> Self {
        Self {
            path,
            grant: LandlockGrant::ReadTree,
        }
    }

    fn list_only(path: PathBuf) -> Self {
        Self {
            path,
            grant: LandlockGrant::ListOnly,
        }
    }

    fn read_file(path: PathBuf) -> Self {
        Self {
            path,
            grant: LandlockGrant::ReadFile,
        }
    }

    /// Whether this grant alone lets the child open `path` for reading.
    pub fn reads(&self, path: &Path) -> bool {
        match self.grant {
            LandlockGrant::ReadTree => path.starts_with(&self.path),
            LandlockGrant::ReadFile => path == self.path,
            LandlockGrant::ListOnly => false,
        }
    }
}

/// Whether any compiled grant lets the child read `path`.
pub fn grants_read(grants: &[LandlockPathGrant], path: &Path) -> bool {
    let path = canonicalize_with_missing_tail(path);
    grants.iter().any(|grant| grant.reads(&path))
}

/// Resolve the existing part of a path before appending any missing names.
///
/// Workspace grants are compiled from canonical paths. A query for a file
/// that has not been created yet cannot itself be canonicalized, so falling
/// back to its original spelling makes an existing symlink alias (such as
/// macOS's `/var` → `/private/var`) look unrelated to the grant. Preserve the
/// same canonical identity for both existing and not-yet-existing paths.
fn canonicalize_with_missing_tail(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut current = path;

    loop {
        if let Ok(canonical) = current.canonicalize() {
            let mut canonical = canonical;
            for component in missing.iter().rev() {
                canonical.push(component);
            }
            return canonical;
        }

        let Some(name) = current.file_name() else {
            return path.to_path_buf();
        };
        missing.push(name.to_os_string());

        let Some(parent) = current.parent() else {
            return path.to_path_buf();
        };
        current = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
    }
}

/// Result of asking the running kernel whether it can enforce a ruleset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandlockProbeOutcome {
    pub available: bool,
    pub abi: i64,
    pub detail: String,
}

/// Ask the running kernel which Landlock ABI it supports.
pub fn probe_landlock() -> LandlockProbeOutcome {
    let abi = landlock_abi();
    if abi >= MINIMUM_LANDLOCK_ABI {
        LandlockProbeOutcome {
            available: true,
            abi,
            detail: format!("Landlock ABI {abi}"),
        }
    } else {
        LandlockProbeOutcome {
            available: false,
            abi,
            detail: unavailable_detail(abi),
        }
    }
}

/// Message for a host that cannot enforce an activity-scoped filesystem
/// profile. Activity-scoped `proc.spawn` fails with this rather than running
/// the child unconfined.
pub fn landlock_unavailable_message(probe: &LandlockProbeOutcome) -> String {
    format!(
        "activity-scoped proc.spawn cannot run here: enforcing the filesystem profile at the \
         process boundary requires Linux Landlock ABI {MINIMUM_LANDLOCK_ABI} or later ({})",
        probe.detail
    )
}

/// The compiled read boundary for one profile: what the child may read, and
/// which of the profile's exclusions the kernel is not holding for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandlockReadBoundary {
    /// Every path grant the ruleset will carry.
    pub grants: Vec<LandlockPathGrant>,
    /// Read exclusions this backend cannot enforce for a path that does not
    /// exist yet, as written in the profile.
    ///
    /// Reported rather than dropped so no layer describes the boundary as
    /// whole-contract enforcement. These rules keep their full effect in the
    /// request-time policy check, which decides a concrete path.
    pub unenforced_exclusions: Vec<String>,
}

/// Compile every path grant a confined child needs: the host paths its own
/// runtime requires, and the workspace paths the resolved profile allows.
///
/// `environment` is the child's own environment, which is what names the tool
/// state directories in [`HOST_READ_ENV_VARS`]. Nothing outside those grants
/// is readable.
pub fn linux_landlock_read_boundary(
    workspace_root: &Path,
    profile: &ResolvedFsProfile,
    environment: &[(String, String)],
) -> Result<LandlockReadBoundary, OrbitError> {
    let workspace_root = workspace_root.canonicalize().map_err(|error| {
        OrbitError::InvalidInput(format!(
            "landlock workspace `{}` must exist and resolve canonically: {error}",
            workspace_root.display()
        ))
    })?;

    let workspace = workspace::workspace_read_grants(&workspace_root, profile)?;
    let mut grants = host::host_read_grants(environment);
    grants.extend(workspace.grants);

    Ok(LandlockReadBoundary {
        grants: dedupe(grants),
        unenforced_exclusions: workspace.unenforced_exclusions,
    })
}

/// Spawn `req` with the child restricted to the compiled ruleset.
///
/// Fails closed: a host that cannot apply Landlock gets a capability error,
/// never an unconfined child.
pub fn spawn_under_linux_landlock(
    req: &ExecRequest,
    workspace_root: &Path,
    profile: &ResolvedFsProfile,
) -> Result<Child, OrbitError> {
    let probe = probe_landlock();
    if !probe.available {
        return Err(OrbitError::PolicyDenied(landlock_unavailable_message(
            &probe,
        )));
    }

    let environment = child_environment(req);
    let boundary = linux_landlock_read_boundary(workspace_root, profile, &environment)?;
    report_unenforced_exclusions(profile, &boundary);

    let mut grants = boundary.grants;
    grants.extend(host::program_grants(&req.program, &environment));
    spawn_restricted(req, &dedupe(grants))
}

/// Record the part of the profile the kernel is not holding for this child.
///
/// A boundary that quietly enforces less than the profile asks for is the
/// failure this module exists to avoid, so the gap is stated once per spawn
/// where an operator reading the run can see it.
fn report_unenforced_exclusions(profile: &ResolvedFsProfile, boundary: &LandlockReadBoundary) {
    if boundary.unenforced_exclusions.is_empty() {
        return;
    }
    tracing::warn!(
        target: "orbit.sandbox.landlock",
        profile = profile.name.as_str(),
        exclusions = boundary.unenforced_exclusions.join(" ").as_str(),
        "landlock cannot enforce these read exclusions for paths that do not exist yet; \
         they remain enforced at request time",
    );
}

/// The environment the child will actually run with, which decides the tool
/// state grants. `Inherit` means the parent's own environment reaches the
/// child, so it is what the grants must be read from.
fn child_environment(req: &ExecRequest) -> Vec<(String, String)> {
    match &req.environment_mode {
        EnvironmentMode::ClearAndSet(pairs) => pairs.clone(),
        EnvironmentMode::Inherit => std::env::vars().collect(),
    }
}

fn dedupe(grants: Vec<LandlockPathGrant>) -> Vec<LandlockPathGrant> {
    let mut seen = BTreeSet::new();
    grants
        .into_iter()
        .filter(|grant| seen.insert((grant.path.clone(), grant.grant)))
        .collect()
}

#[cfg(target_os = "linux")]
fn landlock_abi() -> i64 {
    ruleset::abi_version()
}

#[cfg(not(target_os = "linux"))]
fn landlock_abi() -> i64 {
    -1
}

#[cfg(target_os = "linux")]
fn unavailable_detail(abi: i64) -> String {
    if abi >= 1 {
        format!("this kernel supports only Landlock ABI {abi}")
    } else {
        format!(
            "landlock_create_ruleset version probe failed: {}",
            std::io::Error::last_os_error()
        )
    }
}

#[cfg(not(target_os = "linux"))]
fn unavailable_detail(_abi: i64) -> String {
    format!(
        "Landlock is a Linux facility and {} is not Linux",
        std::env::consts::OS
    )
}

#[cfg(target_os = "linux")]
fn spawn_restricted(req: &ExecRequest, grants: &[LandlockPathGrant]) -> Result<Child, OrbitError> {
    ruleset::spawn_restricted(req, grants)
}

#[cfg(not(target_os = "linux"))]
fn spawn_restricted(
    _req: &ExecRequest,
    _grants: &[LandlockPathGrant],
) -> Result<Child, OrbitError> {
    Err(OrbitError::PolicyDenied(landlock_unavailable_message(
        &probe_landlock(),
    )))
}

#[cfg(test)]
#[path = "tests/mod.rs"]
mod tests;
