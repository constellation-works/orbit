use orbit_config::{ConfigRoots, ResolvedConfig, WorkerContainmentSettings};
use orbit_core::OrbitRuntime;

use super::{WorkspaceDoctorResult, WorkspaceDoctorStatus};

/// Report the `machine.worker_*` limits detached workers launch under, so an
/// operator sees whether a runaway leaf is bounded in memory and CPU.
pub(super) fn worker_containment_row(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let settings = ResolvedConfig::load(&ConfigRoots::new(
        runtime.global_root(),
        runtime.shared_root(),
    ))
    .map(|config| config.snapshot.worker_containment());
    worker_containment_row_for(
        settings.map_err(|error| error.to_string()),
        cfg!(target_os = "linux"),
    )
}

fn worker_containment_row_for(
    settings: Result<WorkerContainmentSettings, String>,
    linux: bool,
) -> WorkspaceDoctorResult {
    let (status, message, remediation) = match settings {
        Err(error) => (
            WorkspaceDoctorStatus::Warning,
            format!("cannot inspect machine.worker_* limits: {error}"),
            Some("Repair the config reported by `orbit doctor`, then rerun it.".to_string()),
        ),
        Ok(_) if !linux => (
            WorkspaceDoctorStatus::Skipped,
            "worker scopes need Linux with a systemd user manager; workers run uncontained here"
                .to_string(),
            None,
        ),
        Ok(settings) if !settings.enabled => (
            WorkspaceDoctorStatus::Info,
            "machine.worker_containment is false: workers share the launcher's cgroup, with no \
             memory, task or CPU limit"
                .to_string(),
            None,
        ),
        Ok(settings) => {
            let memory = format!(
                "MemoryHigh={}, MemoryMax={}, TasksMax={}",
                settings.memory_high.systemd_value(),
                settings.memory_max.systemd_value(),
                settings.tasks_max,
            );
            if settings.cpu_quota_percent == 0 {
                (
                    WorkspaceDoctorStatus::Info,
                    format!(
                        "workers run in a bounded scope ({memory}); CPUQuota is not set, so one worker can use every core"
                    ),
                    Some(
                        "Set machine.worker_cpu_quota to a percentage of one core (for example \
                         400) with `orbit config set --global`."
                            .to_string(),
                    ),
                )
            } else {
                (
                    WorkspaceDoctorStatus::Ok,
                    format!(
                        "workers run in a bounded scope ({memory}, CPUQuota={}%)",
                        settings.cpu_quota_percent
                    ),
                    None,
                )
            }
        }
    };
    WorkspaceDoctorResult {
        duration_ms: 0,
        check_name: "worker-containment".to_string(),
        status,
        message,
        remediation,
    }
}
