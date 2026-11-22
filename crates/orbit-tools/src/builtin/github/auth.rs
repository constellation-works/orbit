use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_exec::ExecRequest;
use serde_json::{Value, json};

use crate::TIMEOUT_DEFAULT_MS;

pub fn build_exec_request(_input: &Value) -> Result<ExecRequest, OrbitError> {
    Ok(super::gh_exec_request(
        vec!["auth".to_string(), "status".to_string()],
        None,
        TIMEOUT_DEFAULT_MS,
    ))
}

/// The preflight answer, in the three shapes a caller has to tell apart.
///
/// A caller that cannot reach GitHub must report *that*, not "no failures
/// found" — so `available` (is there a GitHub CLI at all) and `authenticated`
/// (does it hold usable credentials here) are separate fields, and neither
/// failure mode is an error return. An error would be indistinguishable from
/// the tool itself being broken.
fn unavailable(detail: String) -> Value {
    json!({
        "available": false,
        "authenticated": false,
        "stdout": "",
        "stderr": "",
        "detail": detail,
        "quota": quota_status(),
    })
}

/// The host's GitHub quota record for this lane's account, or `null` when this
/// process does not guard its `gh` reads.
fn quota_status() -> Value {
    crate::github_quota::host_quota_status()
        .and_then(|status| serde_json::to_value(status).ok())
        .unwrap_or(Value::Null)
}

super::gh_tool! {
    pub struct GithubAuthStatusTool;
    name: "github.auth.status";
    description: "Preflight the GitHub CLI surface: whether a GitHub CLI is present on this execution lane (`available`) and whether it holds usable credentials there (`authenticated`). Neither absence nor a missing credential is an error. Quota fields and detail: the orbit skill's tool-surface.md reference, section `GitHub auth status`.";
    parameters: [];
    execute: |_ctx, input| {
        let req = build_exec_request(&input)?;
        // A missing `gh`, or a sandbox that denies executing it, surfaces here
        // as a spawn error. That is a capability answer, not a tool fault.
        let result = match orbit_exec::run_process(&req, &orbit_exec::NoSandbox) {
            Ok(result) => result,
            Err(error) => {
                return Ok(unavailable(redact_all(&error.to_string())));
            }
        };

        Ok(json!({
            "available": true,
            "authenticated": result.success,
            "stdout": redact_all(&result.stdout),
            "stderr": redact_all(&result.stderr),
            "detail": if result.success {
                "GitHub CLI is authenticated on this execution lane"
            } else {
                "GitHub CLI is present but holds no usable credentials on this execution lane"
            },
            "quota": quota_status(),
        }))
    }
}
