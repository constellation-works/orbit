//! Fault-injected native service state, including a stuck oneshot tick.

use std::path::Path;
use std::time::{Duration, SystemTime};

use super::super::manager::{
    ClockCommandRunner, ClockPlatform, ManagerCommand, ManagerCommandOutput,
};
use super::super::status::{LaunchdHealthProbe, clock_status_with, doctor_clock_not_running_issue};
use orbit_automation::routines::sweep::TICK_DEADLINE;
use orbit_common::OrbitError;

struct Manager {
    enabled: bool,
    service: String,
    next_elapse: &'static str,
}
impl ClockCommandRunner for Manager {
    fn run(&self, _: &ManagerCommand) -> Result<bool, OrbitError> {
        unreachable!()
    }
    fn probe(&self, _: &ManagerCommand) -> Result<ManagerCommandOutput, OrbitError> {
        Ok(ManagerCommandOutput {
            success: self.enabled,
            exit_code: Some(if self.enabled { 0 } else { 1 }),
            stdout: if self.enabled { "enabled" } else { "disabled" }.into(),
            stderr: String::new(),
        })
    }
    fn stdout(&self, command: &ManagerCommand) -> Result<Option<String>, OrbitError> {
        if command.args.iter().any(|arg| arg == "orbit-sweep.service") {
            return Ok(Some(self.service.clone()));
        }
        Ok(Some(format!(
            "LoadState=loaded\nActiveState=active\nNextElapseUSecMonotonic={}\n",
            self.next_elapse
        )))
    }
}

#[cfg(unix)]
#[test]
fn active_timer_does_not_hide_an_overdue_tick_or_a_failed_tick() {
    let root = tempfile::tempdir().unwrap();
    let now = 1_000_000_000;
    let budget = u64::try_from(TICK_DEADLINE.as_micros()).unwrap();
    let start = now - budget - 1_000_000;
    for (enabled, service, health_issue, schedulable) in [
        (
            true,
            format!("ActiveState=activating\nExecMainStartTimestampMonotonic={start}\nResult=success\nTimeoutStartUSec=10min\nKillMode=mixed\n"),
            true,
            false,
        ),
        (
            true,
            "ActiveState=inactive\nExecMainStartTimestampMonotonic=0\nResult=timeout\nTimeoutStartUSec=10min\nKillMode=mixed\n".into(),
            true,
            false,
        ),
        (
            true,
            "ActiveState=inactive\nResult=success\nTimeoutStartUSec=infinity\nKillMode=process\n".into(),
            true,
            false,
        ),
        (
            true,
            format!("ActiveState=activating\nExecMainStartTimestampMonotonic={now}\nResult=success\nTimeoutStartUSec=10min\nKillMode=mixed\n"),
            false,
            true,
        ),
        (
            false,
            format!("ActiveState=active\nExecMainStartTimestampMonotonic={now}\nResult=success\nTimeoutStartUSec=10min\nKillMode=mixed\n"),
            true,
            true,
        ),
    ] {
        let status = clock_status_with(
            root.path(),
            ClockPlatform::Systemd,
            &Manager {
                enabled,
                service,
                next_elapse: "1000000",
            },
            None,
            Some(now),
        )
        .unwrap();
        assert_eq!(status.health_issue.is_some(), health_issue);
        assert_eq!(status.schedulable, schedulable);
        assert_eq!(status.effective_cadence_seconds.is_none(), !schedulable);
        if !enabled {
            assert!(status.health_issue.unwrap().contains(
                "systemd timer is disabled but still active"
            ));
        }
    }
}

#[cfg(unix)]
#[test]
fn active_timer_with_infinite_next_elapse_waits_for_the_service_tick() {
    let root = tempfile::tempdir().unwrap();
    for (active_state, schedulable) in [("activating", true), ("active", true), ("inactive", false)]
    {
        let status = clock_status_with(
            root.path(),
            ClockPlatform::Systemd,
            &Manager {
                enabled: true,
                service: format!(
                    "ActiveState={active_state}\nExecMainStartTimestampMonotonic=0\nResult=success\nTimeoutStartUSec=10min\nKillMode=mixed\n"
                ),
                next_elapse: "infinity",
            },
            None,
            Some(1_000_000),
        )
        .unwrap();

        assert_eq!(status.schedulable, schedulable, "service {active_state}");
        assert_eq!(
            status.health_issue.is_none(),
            schedulable,
            "service {active_state}"
        );
        if !schedulable {
            assert!(status.health_issue.unwrap().contains(
                "systemd timer is enabled but is not active with a finite future trigger"
            ));
        }
    }
}

/// Substitute `launchctl` plus temporary sweep logs. Guards the doctor signal
/// for an unloaded agent, a log beyond the tick deadline, duration variance,
/// rotation gaps, and a never-written initial log.
#[cfg(unix)]
#[test]
fn launchd_doctor_readiness_flags_an_unloaded_agent_or_a_stale_sweep_log() {
    let status_for = |root: &Path, loaded| {
        let probe = LaunchdHealthProbe::at(root).unwrap();
        clock_status_with(
            root,
            ClockPlatform::Launchd,
            &Launchctl { loaded },
            Some(&probe),
            None,
        )
        .unwrap()
    };

    for (name, loaded, stale) in [
        ("not loaded", false, false),
        ("stale log", true, true),
        ("loaded and fresh", true, false),
    ] {
        let root = tempfile::tempdir().unwrap();
        let status = status_for(root.path(), loaded);
        let age = if stale {
            Duration::from_secs(status.configured_cadence_seconds)
                .saturating_add(TICK_DEADLINE)
                .saturating_add(Duration::from_secs(1))
        } else {
            Duration::ZERO
        };
        write_sweep_log(root.path(), age);
        let issue = doctor_clock_not_running_issue(&status, root.path());
        match (loaded, stale) {
            (false, _) => {
                let issue = issue.expect(name);
                assert!(issue.contains("not loaded"), "{name}: {issue}");
                assert!(issue.contains("orbit clock enable"), "{name}: {issue}");
                assert!(!issue.contains("older than"), "{name}: {issue}");
            }
            (true, true) => {
                let issue = issue.expect(name);
                assert!(issue.contains("older than"), "{name}: {issue}");
                assert!(issue.contains("sweep.log"), "{name}: {issue}");
                assert!(issue.contains("orbit clock enable"), "{name}: {issue}");
                assert!(!issue.contains("not loaded"), "{name}: {issue}");
            }
            (true, false) => {
                assert!(issue.is_none(), "{name}: {issue:?}");
                assert!(status.enabled, "{name}");
                assert!(status.health_issue.is_none(), "{name}: {status:?}");
            }
        }
    }

    let root = tempfile::tempdir().unwrap();
    let status = status_for(root.path(), true);
    assert!(status.loaded && status.enabled);
    write_sweep_log(
        root.path(),
        Duration::from_secs(status.configured_cadence_seconds)
            .saturating_add(Duration::from_secs(1)),
    );
    assert!(doctor_clock_not_running_issue(&status, root.path()).is_none());

    let root = tempfile::tempdir().unwrap();
    let status = status_for(root.path(), true);
    assert!(status.loaded && status.enabled);
    write_rotated_sweep_log(root.path(), Duration::ZERO);
    assert!(doctor_clock_not_running_issue(&status, root.path()).is_none());

    let root = tempfile::tempdir().unwrap();
    let status = status_for(root.path(), true);
    let issue = doctor_clock_not_running_issue(&status, root.path()).expect("missing first log");
    assert!(issue.contains("sweep.log has not been written"), "{issue}");
}

struct Launchctl {
    loaded: bool,
}

impl ClockCommandRunner for Launchctl {
    fn run(&self, command: &ManagerCommand) -> Result<bool, OrbitError> {
        self.probe(command).map(|output| output.success)
    }

    fn stdout(&self, command: &ManagerCommand) -> Result<Option<String>, OrbitError> {
        let output = self.probe(command)?;
        Ok(output.success.then_some(output.stdout))
    }

    fn probe(&self, command: &ManagerCommand) -> Result<ManagerCommandOutput, OrbitError> {
        let display = command.display();
        if display.contains("list com.orbit.sweep") {
            if self.loaded {
                return Ok(launchctl_output(true, "123\t0\tcom.orbit.sweep\n", ""));
            }
            return Ok(launchctl_output(
                false,
                "",
                "Could not find service \"com.orbit.sweep\" in domain",
            ));
        }
        if display.contains("print ") {
            return Ok(launchctl_output(true, "last exit code = 0\n", ""));
        }
        Err(OrbitError::Execution(format!(
            "unexpected launchctl command {display}"
        )))
    }
}

fn launchctl_output(success: bool, stdout: &str, stderr: &str) -> ManagerCommandOutput {
    ManagerCommandOutput {
        success,
        exit_code: Some(if success { 0 } else { 113 }),
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    }
}

fn write_sweep_log(root: &Path, age: Duration) {
    let path = root.join("logs").join("sweep.log");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = std::fs::File::create(&path).unwrap();
    if age > Duration::ZERO {
        file.set_modified(SystemTime::now().checked_sub(age).unwrap())
            .unwrap();
    }
}

fn write_rotated_sweep_log(root: &Path, age: Duration) {
    let path = root.join("logs").join("sweep.log.20261021T000000000Z");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = std::fs::File::create(&path).unwrap();
    file.set_modified(SystemTime::now().checked_sub(age).unwrap())
        .unwrap();
}
