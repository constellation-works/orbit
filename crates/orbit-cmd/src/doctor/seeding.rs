//! The `seeding-policy` rows: what a workspace's `seeding.routines` and
//! `seeding.auto_tasks` leave out.

use orbit_config::SeedingPolicy;
use orbit_core::OrbitRuntime;
use orbit_core::application::seeding_policy::SeedingPolicyExclusions;

use super::{WorkspaceDoctorResult, WorkspaceDoctorStatus};

const CHECK: &str = "seeding-policy";

/// No row under the default policies, which seed what Orbit always has.
pub(super) fn doctor_check_seeding_policy(runtime: &OrbitRuntime) -> Vec<WorkspaceDoctorResult> {
    let policies = match runtime.workspace_seeding_policies() {
        Ok(policies) => policies,
        Err(error) => {
            return vec![row(
                WorkspaceDoctorStatus::Warning,
                format!("cannot read the workspace seeding policy: {error}"),
                Some("Fix `seeding.routines` or `seeding.auto_tasks` in config.toml.".to_string()),
            )];
        }
    };
    if policies.is_default() {
        return Vec::new();
    }
    let left_out = SeedingPolicyExclusions::of(&policies);
    vec![row(
        WorkspaceDoctorStatus::Info,
        format!(
            "seeding.routines = {}, seeding.auto_tasks = {}: this workspace leaves out {} \
             shipped default routine(s) and {} shipped default auto-task(s), which are not \
             reported missing.",
            label(&policies.routines),
            label(&policies.auto_tasks),
            left_out.routines.len(),
            left_out.auto_tasks.len(),
        ),
        None,
    )]
}

fn label(policy: &SeedingPolicy) -> String {
    policy.to_json().to_string()
}

fn row(
    status: WorkspaceDoctorStatus,
    message: String,
    remediation: Option<String>,
) -> WorkspaceDoctorResult {
    WorkspaceDoctorResult {
        duration_ms: 0,
        check_name: CHECK.into(),
        status,
        message,
        remediation,
    }
}
