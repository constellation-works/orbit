use super::{WorkspaceDoctorResult, WorkspaceDoctorStatus};
use orbit_core::{OrbitError, OrbitRuntime};

/// Run the scan a sandboxed leaf meets at launch on the registered checkout,
/// so an operator sees a refusal before a drain admits work.
pub(super) fn git_protection_row(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let (status, message, remediation) = match runtime.check_git_protection() {
        Ok(()) => (
            WorkspaceDoctorStatus::Ok,
            "Git metadata passes the sandbox protection scan".to_string(),
            None,
        ),
        Err(OrbitError::PolicyDenied(reason)) => (
            WorkspaceDoctorStatus::Warning,
            format!("sandboxed leaves would be refused: {reason}"),
            Some(
                "Remove or repair the entry named above (a leftover `tmp_*` object file can be \
                 deleted; run `git fsck` afterwards), then rerun `orbit doctor`."
                    .to_string(),
            ),
        ),
        Err(error) => (
            WorkspaceDoctorStatus::Warning,
            format!("cannot scan Git metadata: {error}"),
            Some("Fix the access error named above, then rerun `orbit doctor`.".to_string()),
        ),
    };
    WorkspaceDoctorResult {
        duration_ms: 0,
        check_name: "git-protection".to_string(),
        status,
        message,
        remediation,
    }
}
