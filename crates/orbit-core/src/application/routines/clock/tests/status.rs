//! Fault-injected native service state, including a stuck oneshot tick.

use super::super::manager::{
    ClockCommandRunner, ClockPlatform, ManagerCommand, ManagerCommandOutput,
};
use super::super::status::clock_status_with;
use orbit_automation::routines::sweep::TICK_DEADLINE;
use orbit_common::OrbitError;

struct Manager {
    service: String,
    next_elapse: &'static str,
}
impl ClockCommandRunner for Manager {
    fn run(&self, _: &ManagerCommand) -> Result<bool, OrbitError> {
        unreachable!()
    }
    fn probe(&self, _: &ManagerCommand) -> Result<ManagerCommandOutput, OrbitError> {
        Ok(ManagerCommandOutput {
            success: true,
            exit_code: Some(0),
            stdout: "enabled".into(),
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
    for (service, unhealthy) in [
        (format!("ActiveState=activating\nExecMainStartTimestampMonotonic={start}\nResult=success\nTimeoutStartUSec=10min\nKillMode=mixed\n"), true),
        ("ActiveState=inactive\nExecMainStartTimestampMonotonic=0\nResult=timeout\nTimeoutStartUSec=10min\nKillMode=mixed\n".into(), true),
        ("ActiveState=inactive\nResult=success\nTimeoutStartUSec=infinity\nKillMode=process\n".into(), true),
        (format!("ActiveState=activating\nExecMainStartTimestampMonotonic={now}\nResult=success\nTimeoutStartUSec=10min\nKillMode=mixed\n"), false),
    ] {
        let status = clock_status_with(
            root.path(),
            ClockPlatform::Systemd,
            &Manager {
                service,
                next_elapse: "1000000",
            },
            None,
            Some(now),
        )
        .unwrap();
        assert_eq!(status.health_issue.is_some(), unhealthy);
        assert_eq!(status.schedulable, !unhealthy);
        assert_eq!(status.effective_cadence_seconds.is_none(), unhealthy);
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
