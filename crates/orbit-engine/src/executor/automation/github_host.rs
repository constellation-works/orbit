//! Shared host-side GitHub CLI queries for the CI and security-alert sweeps.
//!
//! These engine-private reads use the host's GitHub credentials through the
//! cleared VCS allowlist and `NoSandbox`; only bounded, redacted evidence
//! leaves the sweep boundary. Request construction remains owned by
//! `orbit_tools::github_cli`.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::security::redaction::{redact_all, redact_all_error};
use orbit_exec::{EnvironmentMode, NoSandbox, run_process};
use orbit_tools::github_cli;
use serde_json::Value;

use crate::context::RuntimeHost;

use super::ci::AuthStatus;
use super::input::{canonicalize_existing_dir, input_string_field};
use super::vcs::git::gh_environment;

/// Point one sweep `gh` request at the checkout and the cleared GitHub
/// environment. `github_cli` builds requests with `EnvironmentMode::Inherit`;
/// every host launch replaces that before the process starts.
fn bind_sweep_gh(repo_root: &Path, request: &mut orbit_exec::ExecRequest) {
    request.current_dir = Some(repo_root.to_string_lossy().into_owned());
    request.environment_mode = EnvironmentMode::ClearAndSet(gh_environment());
}

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
    request: orbit_exec::ExecRequest,
    label: &str,
) -> Result<String, OrbitError> {
    let result = run_sweep_gh(repo_root, request).map_err(redact_all_error)?;
    // A rate limit stays typed, so the sweep can wait for its reset.
    github_cli::check_gh_read_result(&result, label).map_err(redact_all_error)?;
    Ok(result.stdout)
}

/// Run one sweep `gh` request under the cleared environment and return its
/// outcome, including a failed read. [`run_gh`] requires success; alert-family
/// queries classify a failure as capability evidence instead.
pub(super) fn run_sweep_gh(
    repo_root: &Path,
    mut request: orbit_exec::ExecRequest,
) -> Result<orbit_exec::ExecutionResult, OrbitError> {
    bind_sweep_gh(repo_root, &mut request);
    run_process(&request, &NoSandbox)
}

/// Probe host authentication, preserving each sweep's unavailable-detail prefix.
pub(super) fn auth_status(repo_root: &Path, unavailable_prefix: &str) -> AuthStatus {
    let request = match github_cli::auth_status_request(&Value::Null) {
        Ok(request) => request,
        Err(error) => {
            return AuthStatus {
                available: false,
                authenticated: false,
                detail: format!("{unavailable_prefix}{}", redact_all(&error.to_string())),
            };
        }
    };
    // A missing `gh`, or a host that refuses to execute it, surfaces as a
    // spawn error. That is a capability answer, not a fault.
    match run_sweep_gh(repo_root, request) {
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
