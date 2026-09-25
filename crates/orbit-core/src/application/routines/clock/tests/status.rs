use tempfile::tempdir;

use orbit_common::OrbitError;

use super::super::inspect::probe_program_version;
use super::super::manager::ClockPlatform;
use super::super::status::clock_status_with;
use super::support::{
    INSTALLED_PROGRAM, MockRunner, installed_version, launchctl_print, launchd_probe,
    manager_output, write_launchd_unit,
};

#[test]
fn status_is_deterministic_for_each_manager_and_reports_configured_cadence() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    write_launchd_unit(home.path(), INSTALLED_PROGRAM);
    let launchd = MockRunner::with_probes(
        Vec::new(),
        Vec::new(),
        vec![
            Ok(manager_output(true, "", "")),
            Ok(manager_output(
                true,
                &launchctl_print("last exit code = 0", "runatload | inferred program"),
                "",
            )),
        ],
    );
    let status = clock_status_with(
        root.path(),
        ClockPlatform::Launchd,
        &launchd,
        Some(&launchd_probe(home.path(), installed_version)),
    )
    .expect("read launchd status");
    assert!(status.enabled);
    assert!(status.schedulable);
    assert!(status.health_issue.is_none());
    assert_eq!(status.configured_cadence_seconds, 60);
    assert_eq!(status.effective_cadence_seconds, Some(60));
    assert_eq!(status.platform, "launchd");
    assert_eq!(
        launchd.commands(),
        vec![
            "launchctl list com.orbit.sweep",
            "launchctl print gui/501/com.orbit.sweep",
        ]
    );

    let systemd = MockRunner::with_outputs(
        vec![Ok(true)],
        vec![Ok(Some(
            "LoadState=loaded\nActiveState=active\nNextElapseUSecRealtime=Sun 2026-08-16 04:30:00 UTC\nNextElapseUSecMonotonic=5min\nLastTriggerUSec=Sun 2026-08-16 04:29:00 UTC".to_string(),
        ))],
    );
    let status = clock_status_with(root.path(), ClockPlatform::Systemd, &systemd, None)
        .expect("read systemd status");
    assert!(status.enabled);
    assert!(status.schedulable);
    assert_eq!(status.effective_cadence_seconds, Some(60));
    assert!(status.health_issue.is_none());
    assert!(status.loaded);
    assert_eq!(status.running, Some(true));
    assert_eq!(
        status.last_tick_at.as_deref(),
        Some("Sun 2026-08-16 04:29:00 UTC")
    );
    assert_eq!(
        status.next_tick_at.as_deref(),
        Some("Sun 2026-08-16 04:30:00 UTC")
    );
    assert_eq!(status.platform, "systemd");
}

#[test]
fn enabled_systemd_timer_without_a_future_trigger_is_unhealthy() {
    let root = tempdir().expect("create global root");
    let runner = MockRunner::with_outputs(
        vec![Ok(true)],
        vec![Ok(Some(
            "LoadState=loaded\nActiveState=active\nNextElapseUSecRealtime=\nNextElapseUSecMonotonic=0"
                .to_string(),
        ))],
    );

    let status = clock_status_with(root.path(), ClockPlatform::Systemd, &runner, None)
        .expect("read elapsed timer status");

    assert!(status.enabled);
    assert!(!status.schedulable);
    assert_eq!(status.effective_cadence_seconds, None);
    assert!(status.health_issue.as_deref().is_some_and(|issue| {
        issue.contains("finite future trigger") && issue.contains("orbit clock enable")
    }));
    assert_eq!(
        runner.commands(),
        vec![
            "systemctl --user is-enabled orbit-sweep.timer",
            "systemctl --user show orbit-sweep.timer --property=LoadState --property=ActiveState --property=NextElapseUSecRealtime --property=NextElapseUSecMonotonic --property=LastTriggerUSec",
        ]
    );
}

#[test]
fn systemd_monotonic_duration_is_not_a_wall_clock_next_tick() {
    let root = tempdir().expect("create global root");
    let runner = MockRunner::with_outputs(
        vec![Ok(true)],
        vec![Ok(Some(
            "LoadState=loaded\nActiveState=active\nNextElapseUSecRealtime=n/a\nNextElapseUSecMonotonic=4h 14min\nLastTriggerUSec=Sun 2026-09-07 21:00:00 UTC"
                .to_string(),
        ))],
    );

    let status = clock_status_with(root.path(), ClockPlatform::Systemd, &runner, None)
        .expect("read monotonic-only timer status");

    assert!(status.enabled);
    assert!(status.schedulable);
    assert_eq!(status.effective_cadence_seconds, Some(60));
    assert_eq!(
        status.last_tick_at.as_deref(),
        Some("Sun 2026-09-07 21:00:00 UTC")
    );
    assert_eq!(
        status.next_tick_at, None,
        "NextElapseUSecMonotonic is a duration from boot, not the next wall-clock tick"
    );
}

#[test]
fn disabled_systemd_timer_reports_loaded_state_without_becoming_schedulable() {
    let root = tempdir().expect("create global root");
    let runner = MockRunner::with_outputs(
        vec![Ok(false)],
        vec![Ok(Some(
            "LoadState=loaded\nActiveState=inactive\nNextElapseUSecRealtime=Sun 2026-08-16 04:30:00 UTC"
                .to_string(),
        ))],
    );

    let status = clock_status_with(root.path(), ClockPlatform::Systemd, &runner, None)
        .expect("read disabled timer status");

    assert!(!status.enabled);
    assert!(status.loaded);
    assert_eq!(status.running, Some(false));
    assert!(!status.schedulable);
    assert_eq!(status.effective_cadence_seconds, None);
    assert!(status.health_issue.is_none());
}

#[test]
fn unavailable_systemd_manager_fails_status_with_bounded_diagnostics() {
    let root = tempdir().expect("create global root");
    let repeated = "manager transport unavailable ".repeat(100);
    let runner = MockRunner::with_probes(
        Vec::new(),
        vec![Err(OrbitError::Execution(format!(
            "systemctl show failed: {repeated}"
        )))],
        vec![Ok(manager_output(
            false,
            "",
            "Failed to connect to bus: No medium found",
        ))],
    );

    let error = clock_status_with(root.path(), ClockPlatform::Systemd, &runner, None)
        .expect_err("unavailable user manager must fail status");
    let message = error.to_string();

    assert!(message.contains("systemd clock manager is unavailable"));
    assert!(message.contains("Failed to connect to bus: No medium found"));
    assert!(message.contains("systemctl show failed"));
    assert!(message.len() < 1_500, "manager diagnostics stay bounded");
    for misleading in [
        "paused",
        "effectively inactive",
        "orbit clock enable",
        "pause",
    ] {
        assert!(!message.contains(misleading));
    }
}

#[test]
fn missing_systemd_unit_is_disabled_but_manager_transport_failure_is_unavailable() {
    let root = tempdir().expect("create global root");
    let missing = MockRunner::with_probes(
        Vec::new(),
        vec![Err(OrbitError::Execution(
            "show failed: Unit orbit-sweep.timer could not be found".to_string(),
        ))],
        vec![Ok(manager_output(
            false,
            "",
            "Failed to get unit file state: No such file or directory",
        ))],
    );

    let status = clock_status_with(root.path(), ClockPlatform::Systemd, &missing, None)
        .expect("a missing unit is a recognized disabled clock");
    assert!(!status.enabled);
    assert!(!status.loaded);
    assert!(!status.schedulable);

    let unavailable = MockRunner::with_probes(
        Vec::new(),
        vec![Err(OrbitError::Execution(
            "show failed: Access denied".to_string(),
        ))],
        vec![Ok(manager_output(false, "", "Access denied"))],
    );
    let error = clock_status_with(root.path(), ClockPlatform::Systemd, &unavailable, None)
        .expect_err("permission failure is not a disabled clock");
    assert!(error.to_string().contains("manager is unavailable"));
}

#[test]
fn systemd_bus_missing_socket_is_unavailable_not_disabled() {
    let root = tempdir().expect("create global root");
    let runner = MockRunner::with_probes(
        Vec::new(),
        vec![Err(OrbitError::Execution(
            "show failed: Failed to connect to bus: No such file or directory".to_string(),
        ))],
        vec![Ok(manager_output(
            false,
            "",
            "Failed to connect to bus: No such file or directory",
        ))],
    );

    let error = clock_status_with(root.path(), ClockPlatform::Systemd, &runner, None)
        .expect_err("a missing user bus socket must fail status, not report a paused clock");
    let message = error.to_string();

    assert!(message.contains("systemd clock manager is unavailable"));
    assert!(message.contains("Failed to connect to bus: No such file or directory"));
    assert!(!message.contains("paused"));
}

#[test]
fn enabled_systemd_with_unavailable_details_remains_enabled_but_unverifiable() {
    let root = tempdir().expect("create global root");
    let runner = MockRunner::with_outputs(
        vec![Ok(true)],
        vec![Err(OrbitError::Execution(
            "systemctl show failed: temporary manager error".to_string(),
        ))],
    );

    let status = clock_status_with(root.path(), ClockPlatform::Systemd, &runner, None)
        .expect("enabled state remains authoritative when only details fail");

    assert!(status.enabled);
    assert!(!status.schedulable);
    assert_eq!(status.effective_cadence_seconds, None);
    assert!(status.health_issue.as_deref().is_some_and(|issue| {
        issue.contains("could not be verified") && issue.contains("temporary manager error")
    }));
}

#[test]
fn launchd_not_loaded_is_disabled_but_transport_failure_is_unavailable() {
    let root = tempdir().expect("create global root");
    let not_loaded = MockRunner::with_probes(
        Vec::new(),
        Vec::new(),
        vec![Ok(manager_output(
            false,
            "",
            "Could not find service com.orbit.sweep in domain for user",
        ))],
    );

    let status = clock_status_with(root.path(), ClockPlatform::Launchd, &not_loaded, None)
        .expect("a recognized not-loaded agent is disabled");
    assert!(!status.enabled);
    assert!(!status.loaded);
    assert!(!status.schedulable);
    assert_eq!(
        not_loaded.commands(),
        vec!["launchctl list com.orbit.sweep"]
    );

    let unavailable = MockRunner::with_probes(
        Vec::new(),
        Vec::new(),
        vec![
            Ok(manager_output(false, "", "Operation not permitted")),
            Ok(manager_output(false, "", "Operation not permitted")),
        ],
    );
    let error = clock_status_with(root.path(), ClockPlatform::Launchd, &unavailable, None)
        .expect_err("failure at the label and manager probes is unavailable");
    let message = error.to_string();
    assert!(message.contains("launchd clock manager is unavailable"));
    assert!(message.contains("launchctl list com.orbit.sweep"));
    assert!(message.contains("launchctl list`"));
    assert!(!message.contains("pause"));
    assert_eq!(
        unavailable.commands(),
        vec!["launchctl list com.orbit.sweep", "launchctl list"]
    );
}

/// The observed agent-sandbox denial: `launchctl list` exits 1 with no output
/// at all, while `launchctl print gui/<uid>/<label>` still answers, so status
/// must trust the per-service dump instead of declaring the manager
/// unavailable [DANI-10519]. The dump is reused for the health check rather
/// than asking launchd a second time.
#[test]
fn sandboxed_launchctl_list_denial_falls_back_to_launchctl_print() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    write_launchd_unit(home.path(), INSTALLED_PROGRAM);
    let runner = MockRunner::with_probes(
        Vec::new(),
        Vec::new(),
        vec![
            Ok(manager_output(false, "", "")),
            Ok(manager_output(
                true,
                &launchctl_print("last exit code = 0", "runatload | inferred program"),
                "",
            )),
        ],
    );

    let status = clock_status_with(
        root.path(),
        ClockPlatform::Launchd,
        &runner,
        Some(&launchd_probe(home.path(), installed_version)),
    )
    .expect("a loaded agent visible through launchctl print is not unavailable");

    assert!(status.enabled);
    assert!(status.loaded);
    assert!(status.schedulable);
    assert!(status.health_issue.is_none());
    assert_eq!(
        runner.commands(),
        vec![
            "launchctl list com.orbit.sweep",
            "launchctl print gui/501/com.orbit.sweep",
        ]
    );
}

/// The same denial with a stalled agent: the dump that proved the agent is
/// loaded also carries the failure launchd recorded, so the health issue is
/// still surfaced without a second `launchctl print`.
#[test]
fn sandboxed_launchctl_list_denial_still_reports_launchd_health_from_print() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    write_launchd_unit(home.path(), INSTALLED_PROGRAM);
    let runner = MockRunner::with_probes(
        Vec::new(),
        Vec::new(),
        vec![
            Ok(manager_output(false, "", "")),
            Ok(manager_output(
                true,
                &launchctl_print("last exit code = 78: EX_CONFIG", "penalty box"),
                "",
            )),
        ],
    );

    let status = clock_status_with(
        root.path(),
        ClockPlatform::Launchd,
        &runner,
        Some(&launchd_probe(home.path(), installed_version)),
    )
    .expect("read launchd status");

    assert!(status.enabled);
    assert!(!status.schedulable);
    let issue = status
        .health_issue
        .expect("stalled agent reports a health issue");
    assert!(issue.contains("exited 78 without sweeping"));
    assert!(issue.contains("penalty box"));
    assert_eq!(
        runner.commands(),
        vec![
            "launchctl list com.orbit.sweep",
            "launchctl print gui/501/com.orbit.sweep",
        ]
    );
}

/// A paused or unloaded agent inside the same sandbox: `list` is denied and
/// `print` names the not-loaded state, which is disabled rather than
/// unavailable, and never enabled.
#[test]
fn sandboxed_launchctl_list_denial_with_unloaded_agent_is_not_enabled() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    write_launchd_unit(home.path(), INSTALLED_PROGRAM);
    let runner = MockRunner::with_probes(
        Vec::new(),
        Vec::new(),
        vec![
            Ok(manager_output(false, "", "")),
            Ok(manager_output(
                false,
                "",
                "Could not find service \"com.orbit.sweep\" in domain for uid: 501",
            )),
        ],
    );

    let status = clock_status_with(
        root.path(),
        ClockPlatform::Launchd,
        &runner,
        Some(&launchd_probe(home.path(), installed_version)),
    )
    .expect("an unloaded agent named by launchctl print is disabled");

    assert!(!status.enabled);
    assert!(!status.loaded);
    assert!(!status.schedulable);
    assert!(status.health_issue.is_none());
    assert_eq!(
        runner.commands(),
        vec![
            "launchctl list com.orbit.sweep",
            "launchctl print gui/501/com.orbit.sweep",
        ]
    );

    // When neither probe names a not-loaded state, the bare `launchctl list`
    // still decides between paused and unavailable, and every attempt is
    // named in the diagnostic.
    let unavailable = MockRunner::with_probes(
        Vec::new(),
        Vec::new(),
        vec![
            Ok(manager_output(false, "", "")),
            Ok(manager_output(false, "", "")),
            Ok(manager_output(false, "", "")),
        ],
    );
    let error = clock_status_with(
        root.path(),
        ClockPlatform::Launchd,
        &unavailable,
        Some(&launchd_probe(home.path(), installed_version)),
    )
    .expect_err("silent failure at every probe is unavailable");
    let message = error.to_string();
    assert!(message.contains("launchd clock manager is unavailable"));
    assert!(
        message.contains("`launchctl list com.orbit.sweep` failed with exit code 1 without output")
    );
    assert!(message.contains(
        "`launchctl print gui/501/com.orbit.sweep` failed with exit code 1 without output"
    ));
    assert!(message.contains("`launchctl list` failed with exit code 1 without output"));
    assert_eq!(
        unavailable.commands(),
        vec![
            "launchctl list com.orbit.sweep",
            "launchctl print gui/501/com.orbit.sweep",
            "launchctl list",
        ]
    );
}

/// The observed Mac mini failure: the plist still names a package-manager
/// install that no longer exists, so no sweep can fire even though launchd
/// keeps reporting the agent as loaded [DANI-10386].
#[test]
fn launchd_agent_naming_a_missing_program_is_unhealthy() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    write_launchd_unit(home.path(), INSTALLED_PROGRAM);
    let runner = MockRunner::with_probes(
        Vec::new(),
        Vec::new(),
        vec![Ok(manager_output(true, "", ""))],
    );

    let status = clock_status_with(
        root.path(),
        ClockPlatform::Launchd,
        &runner,
        // The real probe `orbit doctor clock-unit` uses, so both surfaces
        // reach the same verdict for the same unit.
        Some(&launchd_probe(home.path(), probe_program_version)),
    )
    .expect("read launchd status");

    assert!(status.enabled);
    assert!(!status.schedulable);
    assert_eq!(status.effective_cadence_seconds, None);
    let issue = status
        .health_issue
        .expect("a missing program is a health issue");
    assert!(issue.contains("program cannot run"));
    assert!(issue.contains(INSTALLED_PROGRAM));
    assert!(issue.contains("program does not exist"));
    assert!(issue.contains("orbit clock enable"));
    // The transcript is never fetched: the unit file already settled it.
    assert_eq!(runner.commands(), vec!["launchctl list com.orbit.sweep"]);
}

#[test]
fn launchd_agent_with_a_failing_last_exit_code_is_unhealthy() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    write_launchd_unit(home.path(), INSTALLED_PROGRAM);
    let runner = MockRunner::with_probes(
        Vec::new(),
        Vec::new(),
        vec![
            Ok(manager_output(true, "", "")),
            Ok(manager_output(
                true,
                &launchctl_print(
                    "last exit code = 78: EX_CONFIG",
                    "runatload | inferred program",
                ),
                "",
            )),
        ],
    );

    let status = clock_status_with(
        root.path(),
        ClockPlatform::Launchd,
        &runner,
        Some(&launchd_probe(home.path(), installed_version)),
    )
    .expect("read launchd status");

    assert!(status.enabled);
    assert!(!status.schedulable);
    assert_eq!(status.effective_cadence_seconds, None);
    let issue = status.health_issue.expect("a failed run is a health issue");
    assert!(issue.contains("exited 78"));
    assert!(!issue.contains("penalty box"));
    assert!(issue.contains("orbit clock enable"));
    assert_eq!(
        runner.commands(),
        vec![
            "launchctl list com.orbit.sweep",
            "launchctl print gui/501/com.orbit.sweep",
        ]
    );
}

#[test]
fn launchd_agent_in_the_penalty_box_is_unhealthy() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    write_launchd_unit(home.path(), INSTALLED_PROGRAM);
    let runner = MockRunner::with_probes(
        Vec::new(),
        Vec::new(),
        vec![
            Ok(manager_output(true, "", "")),
            Ok(manager_output(
                true,
                &launchctl_print(
                    "last exit code = 0",
                    "runatload | penalty box | inferred program | managed LWCR",
                ),
                "",
            )),
        ],
    );

    let status = clock_status_with(
        root.path(),
        ClockPlatform::Launchd,
        &runner,
        Some(&launchd_probe(home.path(), installed_version)),
    )
    .expect("read launchd status");

    assert!(status.enabled);
    assert!(!status.schedulable);
    let issue = status
        .health_issue
        .expect("a throttled agent is a health issue");
    assert!(issue.contains("penalty box"));
    assert!(!issue.contains("exited"));
    assert!(issue.contains("orbit clock enable"));
}

/// A never-run agent reports `(never exited)`, and `jetsamproperties` must not
/// be mistaken for the `properties` line that carries the penalty box.
#[test]
fn launchd_agent_that_has_not_run_yet_is_healthy() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    write_launchd_unit(home.path(), INSTALLED_PROGRAM);
    let runner = MockRunner::with_probes(
        Vec::new(),
        Vec::new(),
        vec![
            Ok(manager_output(true, "", "")),
            Ok(manager_output(
                true,
                &launchctl_print("last exit code = (never exited)", "runatload | keepalive"),
                "",
            )),
        ],
    );

    let status = clock_status_with(
        root.path(),
        ClockPlatform::Launchd,
        &runner,
        Some(&launchd_probe(home.path(), installed_version)),
    )
    .expect("read launchd status");

    assert!(status.enabled);
    assert!(status.schedulable);
    assert_eq!(status.effective_cadence_seconds, Some(60));
    assert!(status.health_issue.is_none());
}

/// An enabled agent whose transcript cannot be read is reported as degraded
/// rather than healthy, mirroring the systemd arm.
#[test]
fn launchd_agent_with_an_unreadable_transcript_is_unhealthy() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    write_launchd_unit(home.path(), INSTALLED_PROGRAM);
    let runner = MockRunner::with_probes(
        Vec::new(),
        Vec::new(),
        vec![
            Ok(manager_output(true, "", "")),
            Ok(manager_output(false, "", "Could not find service")),
        ],
    );

    let status = clock_status_with(
        root.path(),
        ClockPlatform::Launchd,
        &runner,
        Some(&launchd_probe(home.path(), installed_version)),
    )
    .expect("read launchd status");

    assert!(status.enabled);
    assert!(!status.schedulable);
    let issue = status
        .health_issue
        .expect("an unverifiable agent is a health issue");
    assert!(issue.contains("could not be verified"));
    assert!(issue.contains("launchctl print gui/501/com.orbit.sweep"));
    assert!(issue.contains("orbit clock enable"));
}

/// A paused clock is intentionally not schedulable and is not unhealthy, and
/// a paused host is never probed for a transcript.
#[test]
fn paused_launchd_clock_reports_no_health_issue() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    write_launchd_unit(home.path(), INSTALLED_PROGRAM);
    let runner = MockRunner::with_probes(
        Vec::new(),
        Vec::new(),
        vec![Ok(manager_output(
            false,
            "",
            "Could not find service com.orbit.sweep in domain for user",
        ))],
    );

    let status = clock_status_with(
        root.path(),
        ClockPlatform::Launchd,
        &runner,
        Some(&launchd_probe(home.path(), probe_program_version)),
    )
    .expect("read launchd status");

    assert!(!status.enabled);
    assert!(!status.schedulable);
    assert!(status.health_issue.is_none());
    assert_eq!(runner.commands(), vec!["launchctl list com.orbit.sweep"]);
}
