//! Shared host-side GitHub CLI queries for the CI and security-alert sweeps.
//!
//! These engine-private reads use the host's existing credentials and
//! `NoSandbox`; only bounded, redacted evidence leaves the sweep boundary.
//! Request construction remains owned by `orbit_tools::github_cli`.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_exec::{NoSandbox, run_process};
use orbit_tools::{check_exec_result, github_cli};
use serde_json::Value;

use crate::context::RuntimeHost;

use super::ci::AuthStatus;
use super::input::{canonicalize_existing_dir, input_string_field};

/// A GitHub client was absent or unauthenticated on this host, so no
/// evidence was gathered. Never a clean bill of health.
pub(super) const OUTCOME_CAPABILITY_UNAVAILABLE: &str = "capability_unavailable";

/// Query from the explicit checkout when supplied, otherwise the workspace
/// repository root. Both resolve to the same remote.
pub(super) fn query_root<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<PathBuf, OrbitError> {
    match input_string_field(input, "workspace_path") {
        Some(path) => canonicalize_existing_dir(&path, "workspace_path"),
        None => Ok(PathBuf::from(host.repo_root()?)),
    }
}

pub(super) fn run_gh(
    repo_root: &Path,
    mut request: orbit_exec::ExecRequest,
    label: &str,
) -> Result<String, OrbitError> {
    request.current_dir = Some(repo_root.to_string_lossy().into_owned());
    let result = run_process(&request, &NoSandbox)?;
    check_exec_result(&result, label)?;
    Ok(result.stdout)
}

/// Probe host authentication, preserving each sweep's unavailable-detail prefix.
pub(super) fn auth_status(repo_root: &Path, unavailable_prefix: &str) -> AuthStatus {
    let mut request = match github_cli::auth_status_request(&Value::Null) {
        Ok(request) => request,
        Err(error) => {
            return AuthStatus {
                available: false,
                authenticated: false,
                detail: format!("{unavailable_prefix}{}", redact_all(&error.to_string())),
            };
        }
    };
    request.current_dir = Some(repo_root.to_string_lossy().into_owned());
    // A missing `gh`, or a host that refuses to execute it, surfaces as a
    // spawn error. That is a capability answer, not a fault.
    match run_process(&request, &NoSandbox) {
        Ok(result) if result.success => AuthStatus {
            available: true,
            authenticated: true,
            detail: "GitHub CLI is authenticated on this host".to_string(),
        },
        Ok(result) => AuthStatus {
            available: true,
            authenticated: false,
            detail: format!(
                "GitHub CLI is present but holds no usable credentials on this host: {}",
                redact_all(result.stderr.trim())
            ),
        },
        Err(error) => AuthStatus {
            available: false,
            authenticated: false,
            detail: format!("{unavailable_prefix}{}", redact_all(&error.to_string())),
        },
    }
}
