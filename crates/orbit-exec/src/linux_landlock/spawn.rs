//! Spawning a child under a compiled Landlock ruleset, and the environment
//! that child runs with.

use std::path::Path;
use std::process::Child;

use orbit_common::OrbitError;
use orbit_common::tracing;
use orbit_types::policy::ResolvedFsProfile;

use super::boundary::{
    LandlockBoundary, LandlockReadBoundary, linux_landlock_boundary_grants,
    linux_landlock_read_boundary, plugin_scope,
};
use super::grants::dedupe;
use super::probe::{RulesetScope, landlock_unavailable_message, probe_landlock};
use super::{LandlockPathGrant, host};
use crate::runner::{EnvironmentMode, ExecRequest};

/// Spawn `req` confined to an explicit boundary.
///
/// Fails closed like [`spawn_under_linux_landlock`]: no Landlock, or a
/// write or TCP right the kernel cannot hold, is a capability error and never
/// an unconfined child.
///
/// `inherited_fds` are descriptors the parent hands the child at fixed
/// numbers — the plugin callback credential — mapped between `fork` and
/// `exec` alongside the ruleset.
pub fn spawn_under_linux_landlock_boundary(
    req: &ExecRequest,
    boundary: &LandlockBoundary,
    inherited_fds: &[crate::process::InheritedFd],
) -> Result<Child, OrbitError> {
    let probe = probe_landlock();
    let scope = plugin_scope(&probe, boundary)?;
    let grants = linux_landlock_boundary_grants(req, boundary)?;
    spawn_restricted(req, &grants, scope, inherited_fds)
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
    spawn_restricted(req, &dedupe(grants), RulesetScope::default(), &[])
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
pub(super) fn child_environment(req: &ExecRequest) -> Vec<(String, String)> {
    match &req.environment_mode {
        EnvironmentMode::ClearAndSet(pairs) => pairs.clone(),
        EnvironmentMode::Inherit => std::env::vars().collect(),
    }
}

#[cfg(target_os = "linux")]
fn spawn_restricted(
    req: &ExecRequest,
    grants: &[LandlockPathGrant],
    scope: RulesetScope,
    inherited_fds: &[crate::process::InheritedFd],
) -> Result<Child, OrbitError> {
    super::ruleset::spawn_restricted(req, grants, scope, inherited_fds)
}

#[cfg(not(target_os = "linux"))]
fn spawn_restricted(
    _req: &ExecRequest,
    _grants: &[LandlockPathGrant],
    _scope: RulesetScope,
    _inherited_fds: &[crate::process::InheritedFd],
) -> Result<Child, OrbitError> {
    Err(OrbitError::PolicyDenied(landlock_unavailable_message(
        &probe_landlock(),
    )))
}
