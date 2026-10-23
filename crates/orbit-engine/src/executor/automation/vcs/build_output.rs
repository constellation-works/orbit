//! Toolchain-neutral host output placement. The legacy directory and source
//! key are retained so descriptor-bound GC and scratch lineage still own all
//! previously created output; workers cannot write this host state.

use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_exec::ValidationEnvironment;
use std::path::{Path, PathBuf};

/// Where host validation targets live, relative to the primary checkout.
pub(super) const HOST_VALIDATION_TARGETS_DIR: [&str; 3] =
    [".orbit", "state", "host-validation-targets"];

/// The directory name of the host validation target for a canonical source
/// checkout. Worktree GC re-derives it from run records with this same rule.
pub(super) fn host_validation_target_key(source: &Path) -> String {
    sha256_hex(source.as_os_str().as_encoded_bytes())
}

/// The primary checkout holding `checkout`'s host validation target, and the
/// canonical source path that keys it.
pub(super) fn host_validation_source(checkout: &Path) -> Result<(PathBuf, PathBuf), OrbitError> {
    let source = checkout.canonicalize()?;
    let common = orbit_common::fs::git::git_common_dir(&source)?;
    let primary = common
        .parent()
        .filter(|_| common.file_name().is_some_and(|name| name == ".git"))
        .ok_or_else(|| {
            OrbitError::Execution("host validation needs a primary Git checkout".into())
        })?
        .to_path_buf();
    Ok((primary, source))
}

/// Host-owned build output, separate from every sandbox-writable target.
/// Key by the canonical *source* path: build-tool freshness alone cannot detect
/// compiler-wrapper rewrites of embedded paths, or a different source checkout.
/// The same checkout's repair/retry/resume can safely reuse its host output.
fn host_validation_target(primary: PathBuf, source: &Path) -> Result<PathBuf, OrbitError> {
    let key = host_validation_target_key(source);
    let mut target = primary;
    // These are host state, never global compiler/download caches. Refuse
    // redirects rather than creating output through a sandbox-writable symlink.
    for component in HOST_VALIDATION_TARGETS_DIR
        .iter()
        .copied()
        .chain([key.as_str()])
    {
        target.push(component);
        match std::fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(OrbitError::PolicyDenied(format!(
                    "host validation target component '{}' must be a directory, not a redirect",
                    target.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match orbit_common::fs::io::create_private_dir(&target) {
                    Ok(()) => {}
                    // Another host command may have created the shared parent.
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        let metadata = std::fs::symlink_metadata(&target)?;
                        if !metadata.is_dir() || metadata.file_type().is_symlink() {
                            return Err(OrbitError::PolicyDenied(format!(
                                "host validation target '{}' became a redirect",
                                target.display()
                            )));
                        }
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(target)
}

/// Name of Cargo's incremental-compilation switch. Set on the child only
/// when a host target is bound, so a Makefile or `.cargo/config.toml` is
/// not what turns the cache off.
const CARGO_INCREMENTAL: &str = "CARGO_INCREMENTAL";

/// Apply the workspace mapping after ordinary environment resolution.
/// No configured binding means no allocation and no environment mutation.
///
/// A bound directory is keyed by the checkout and is rebuilt once per
/// candidate, so an incremental cache there is write-only. Required
/// validation, host evidence, baseline reruns, flaky replays and recovery
/// all bind through here. Unless
/// [`ValidationEnvironment::cargo_incremental`] is set, the child receives
/// `CARGO_INCREMENTAL=0`, replacing any inherited value.
pub(super) fn bind_host_output(
    environment: &mut ValidationEnvironment,
    checkout: &Path,
) -> Result<(), OrbitError> {
    environment.build_output_dir = None;
    environment.build_output_source = None;
    if environment.build_output_env.is_empty() {
        return Ok(());
    }
    let (primary, source) = host_validation_source(checkout)?;
    let directory = host_validation_target(primary, &source)?;
    let value = directory.to_string_lossy().into_owned();
    environment
        .env
        .retain(|(name, _)| !environment.build_output_env.contains(name));
    for name in &environment.build_output_env {
        environment.env.push((name.clone(), value.clone()));
    }
    if !environment.cargo_incremental {
        environment
            .env
            .retain(|(name, _)| name != CARGO_INCREMENTAL);
        environment
            .env
            .push((CARGO_INCREMENTAL.to_string(), "0".to_string()));
    }
    environment.build_output_source = Some(source);
    environment.build_output_dir = Some(directory);
    Ok(())
}
