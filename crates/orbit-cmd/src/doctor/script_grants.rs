//! Script grant findings: a workspace script whose grant is missing,
//! stale (either digested file changed after granting), orphaned (the
//! manifest is gone), or refused by its witness. A stale granted script
//! is a warning, per the script-steps design: nothing will run it until
//! an operator re-grants after seeing the change.
//!
//! A workspace that declares scripts also gets a `script-sandbox` row: the
//! profile its steps run under here (or why none can be applied), the
//! macOS profile's documented gaps, and whether `strict` worker containment
//! refuses every step on this host.

use orbit_core::OrbitRuntime;
use orbit_core::runtime::scripts::ScriptGrantState;

use super::{WorkspaceDoctorResult, WorkspaceDoctorStatus};

pub(super) fn script_grant_rows(runtime: &OrbitRuntime) -> Vec<WorkspaceDoctorResult> {
    let statuses = match runtime.script_statuses() {
        Ok(statuses) => statuses,
        Err(error) => {
            return vec![WorkspaceDoctorResult {
                duration_ms: 0,
                check_name: "script-grants".into(),
                status: WorkspaceDoctorStatus::Warning,
                message: format!("Cannot inspect script grants: {error}"),
                remediation: Some(
                    "Fix the script manifest or store error it names, then rerun `orbit doctor`."
                        .into(),
                ),
            }];
        }
    };
    if statuses.is_empty() {
        return vec![WorkspaceDoctorResult {
            duration_ms: 0,
            check_name: "script-grants".into(),
            status: WorkspaceDoctorStatus::Skipped,
            message: "This workspace declares no scripts and holds no script grants.".into(),
            remediation: None,
        }];
    }
    let mut rows: Vec<WorkspaceDoctorResult> = Vec::new();
    let mut granted = 0usize;
    for status in &statuses {
        let (doctor_status, remediation) = match status.state {
            ScriptGrantState::Granted => {
                granted += 1;
                continue;
            }
            ScriptGrantState::Stale => (
                WorkspaceDoctorStatus::Warning,
                Some(format!(
                    "Review the change, then run `orbit script grant {}` to authorize the current bytes.",
                    status.name
                )),
            ),
            ScriptGrantState::Ungranted => (
                WorkspaceDoctorStatus::Warning,
                Some(format!(
                    "Run `orbit script grant {}` to authorize this script on this host.",
                    status.name
                )),
            ),
            ScriptGrantState::Orphaned => (
                WorkspaceDoctorStatus::Warning,
                Some(format!(
                    "Run `orbit script revoke {}` to drop the orphaned grant.",
                    status.name
                )),
            ),
            ScriptGrantState::Unavailable => {
                (WorkspaceDoctorStatus::Warning, Some(status.detail.clone()))
            }
            ScriptGrantState::WitnessRefused => (
                WorkspaceDoctorStatus::Error,
                Some(format!(
                    "Run `orbit script grant {}` to re-authorize, or `orbit script revoke {}` to drop the record.",
                    status.name, status.name
                )),
            ),
        };
        rows.push(WorkspaceDoctorResult {
            duration_ms: 0,
            check_name: "script-grants".into(),
            status: doctor_status,
            message: format!(
                "script `{}` is {}: {}",
                status.name, status.state, status.detail
            ),
            remediation,
        });
    }
    rows.push(script_sandbox_row(runtime));
    if rows.len() == 1 {
        rows.push(WorkspaceDoctorResult {
            duration_ms: 0,
            check_name: "script-grants".into(),
            status: WorkspaceDoctorStatus::Ok,
            message: format!(
                "{granted} script grant{} current at the granted digest.",
                if granted == 1 { " is" } else { "s are" }
            ),
            remediation: None,
        });
    }
    rows
}

fn script_sandbox_row(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let report = runtime.script_sandbox_report();
    let (status, message, remediation) = match report.profile {
        Err(reason) => (
            WorkspaceDoctorStatus::Warning,
            format!(
                "script steps cannot run on this host and refuse with \
                 script_sandbox_unavailable: {reason}"
            ),
            None,
        ),
        Ok(profile) if report.strict_refuses => (
            WorkspaceDoctorStatus::Warning,
            format!(
                "script steps would run under {profile}, but machine.worker_containment is \
                 strict, which refuses every script step on macOS (no kernel containment)"
            ),
            Some(
                "Set machine.worker_containment to warn with `orbit config set --global` to \
                 run script steps under the profile alone."
                    .to_string(),
            ),
        ),
        Ok(profile) => match report.gaps {
            Some(gaps) => (
                WorkspaceDoctorStatus::Info,
                format!("script steps run under {profile}; its gaps against Linux: {gaps}"),
                None,
            ),
            None => (
                WorkspaceDoctorStatus::Ok,
                format!("script steps run under {profile}"),
                None,
            ),
        },
    };
    WorkspaceDoctorResult {
        duration_ms: 0,
        check_name: "script-sandbox".into(),
        status,
        message,
        remediation,
    }
}
