use std::fs;

use tempfile::tempdir;

use super::super::converge::{clock_reload_pending_path, converge_clock_unit_with};
use super::super::install::{
    install_clock_with, render_systemd_service, render_systemd_timer, validated_sweep_log_path,
};
use super::super::manager::ClockPlatform;
use super::super::settings::ClockSettings;
use super::support::{
    INSTALLED_PROGRAM, MockRunner, SystemdManagerFake, finds_launcher, service_path,
    write_launchd_unit,
};

#[test]
fn rendered_systemd_service_discovers_local_provider_launchers() {
    let home = tempdir().expect("create temporary home");
    let provider_bin = home.path().join(".local/bin");
    fs::create_dir_all(&provider_bin).expect("create provider directory");
    fs::write(provider_bin.join("codex"), "provider launcher").expect("write provider launcher");

    let manager_path = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
    assert!(
        !finds_launcher(manager_path, "codex"),
        "the minimal user-manager PATH does not find the provider launcher"
    );

    let rendered = render_systemd_service("/opt/orbit/bin/orbit");
    let rendered_path = service_path(&rendered);
    let effective_path = rendered_path.replace("%h", &home.path().to_string_lossy());

    assert!(
        finds_launcher(&effective_path, "codex"),
        "the service PATH finds a provider launcher from the user's .local/bin"
    );
    assert!(rendered_path.starts_with("%h/.local/bin:%h/.orbit/bin:%h/.cargo/bin:"));
    for directory in [
        "/usr/local/sbin",
        "/usr/local/bin",
        "/usr/sbin",
        "/usr/bin",
        "/sbin",
        "/bin",
    ] {
        assert!(rendered_path.split(':').any(|entry| entry == directory));
    }
    assert!(!rendered_path.contains('~'));
    assert!(!rendered_path.contains("/home/"));
    assert!(!rendered_path.contains("/nix/store/"));
    assert!(rendered.contains("Type=oneshot"));
    assert!(rendered.contains("KillMode=process"));
    // 2026-09-23 OOM outage: an unbounded sweep cgroup let one run take the host.
    assert!(
        rendered.contains("\nMemoryHigh=") && rendered.contains("\nTasksMax="),
        "the sweep unit bounds the work left in its cgroup"
    );
    assert!(
        !rendered.contains("MemoryMax="),
        "a hard limit on the sweep cgroup would OOM-kill in-flight uncontained runs"
    );
    assert!(rendered.contains("ExecStart=/opt/orbit/bin/orbit clock tick"));
}

#[test]
fn systemd_timer_renders_default_and_configured_cadence() {
    let default_timer = render_systemd_timer(ClockSettings::default());
    assert!(default_timer.contains("OnActiveSec=60s"));
    assert!(default_timer.contains("OnUnitActiveSec=60s"));
    assert!(!default_timer.contains("OnStartupSec="));
    assert!(!default_timer.contains("Persistent=true"));

    let configured_timer = render_systemd_timer(ClockSettings {
        cadence_seconds: 300,
    });
    assert!(configured_timer.contains("OnActiveSec=300s"));
    assert!(configured_timer.contains("OnUnitActiveSec=300s"));
}

#[test]
fn systemd_install_arms_and_recurs_at_default_and_configured_cadence() {
    for cadence_seconds in [60, 300] {
        let root = tempdir().expect("create global root");
        let home = tempdir().expect("create home");
        let runner = SystemdManagerFake::new(home.path(), 10);

        let report = install_clock_with(
            root.path(),
            "/opt/orbit/bin/orbit",
            ClockSettings { cadence_seconds },
            ClockPlatform::Systemd,
            &runner,
            home.path(),
        )
        .expect("install systemd clock");

        assert!(report.activated);
        assert_eq!(runner.next_trigger(), Some(10 + cadence_seconds));
        runner.elapse_and_complete_service();
        assert_eq!(
            runner.next_trigger(),
            Some(10 + cadence_seconds * 2),
            "service activation establishes the recurring deadline"
        );
    }
}

#[test]
fn late_reinstall_rearms_an_active_elapsed_timer_from_timer_activation() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    let week = 7 * 24 * 60 * 60;
    let runner = SystemdManagerFake::late_elapsed(home.path(), week, 60);

    let report = install_clock_with(
        root.path(),
        "/opt/orbit/bin/orbit",
        ClockSettings {
            cadence_seconds: 300,
        },
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .expect("late reinstall re-arms timer");

    assert!(report.activated);
    let next = runner
        .next_trigger()
        .expect("finite trigger after reinstall");
    assert!((week + 300..=week + 305).contains(&next));
}

/// A fresh `orbit routine init --install-clock` that activates the unit
/// supersedes any reload an earlier repair left pending.
#[test]
fn install_forgets_a_reload_a_failed_repair_left_pending() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    let running = home.path().join("orbit");
    fs::write(&running, "binary").expect("running binary");
    write_launchd_unit(home.path(), INSTALLED_PROGRAM);
    let repair = MockRunner::new(vec![Ok(true), Ok(true), Ok(false)]);
    converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Launchd,
        &repair,
        home.path(),
    )
    .expect("repair rewrites the unit");
    assert!(clock_reload_pending_path(root.path()).exists());

    let install = MockRunner::new(vec![Ok(true), Ok(true)]);
    let report = install_clock_with(
        root.path(),
        &running.to_string_lossy(),
        ClockSettings::default(),
        ClockPlatform::Launchd,
        &install,
        home.path(),
    )
    .expect("install activates the unit");
    assert!(report.activated);
    assert!(!clock_reload_pending_path(root.path()).exists());
}

#[cfg(unix)]
#[test]
fn sweep_log_path_rejects_symlinked_directory() {
    use std::os::unix::fs::symlink;

    let root = tempdir().expect("create global root");
    let outside = tempdir().expect("create outside root");
    symlink(outside.path(), root.path().join("logs")).expect("create logs symlink");

    let error = validated_sweep_log_path(root.path()).expect_err("reject escaped log directory");

    assert!(
        error
            .to_string()
            .contains("sweep log directory must be a regular directory directly under")
    );
}

#[cfg(unix)]
#[test]
fn sweep_log_path_rejects_symlinked_file() {
    use std::os::unix::fs::symlink;

    let root = tempdir().expect("create global root");
    let outside = tempdir().expect("create outside root");
    fs::create_dir(root.path().join("logs")).expect("create log directory");
    let outside_log = outside.path().join("sweep.log");
    fs::write(&outside_log, "redirected").expect("write outside log");
    symlink(&outside_log, root.path().join("logs/sweep.log")).expect("create log symlink");

    let error = validated_sweep_log_path(root.path()).expect_err("reject escaped log file");

    assert!(
        error
            .to_string()
            .contains("sweep log must be a regular file directly under")
    );
}

#[test]
fn systemd_install_rejects_successful_commands_without_a_finite_trigger() {
    let root = tempdir().expect("create global root");
    let home = tempdir().expect("create home");
    let runner = MockRunner::with_outputs(
        vec![Ok(true), Ok(true), Ok(true)],
        vec![Ok(Some(
            "LoadState=loaded\nActiveState=active\nNextElapseUSecMonotonic=infinity".to_string(),
        ))],
    );

    let error = install_clock_with(
        root.path(),
        "/opt/orbit/bin/orbit",
        ClockSettings::default(),
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .expect_err("unschedulable activation is not reported as active");

    assert!(error.to_string().contains("finite future trigger"));
    assert!(error.to_string().contains("systemctl --user status"));
    assert!(error.to_string().contains("journalctl --user"));
    assert!(
        !error.to_string().contains("orbit clock enable"),
        "a completed enable/install that is still unschedulable must not tell the operator to repeat enable"
    );
}
