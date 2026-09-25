use std::fs;
use std::path::Path;

use tempfile::tempdir;

use super::super::converge::{
    ClockUnitConvergence, ClockUnitDrift, clock_reload_pending_path, clock_unit_drift_warning_at,
    converge_clock_unit_with,
};
use super::super::manager::ClockPlatform;
use super::inspect::{write_launchd_unit, write_systemd_unit};
use super::support::MockRunner;

/// A stale unit whose program was deleted — the launchd penalty-box failure —
/// is rewritten to the running binary and re-registered.
#[test]
fn launchd_converge_rewrites_a_unit_whose_program_no_longer_exists() {
    let root = tempdir().expect("global root");
    let home = tempdir().expect("home");
    let running = home.path().join("orbit/bin/orbit");
    fs::create_dir_all(running.parent().expect("parent")).expect("running parent");
    fs::write(&running, "binary").expect("running binary");
    let removed = home.path().join("homebrew/bin/orbit");
    let unit = write_launchd_unit(home.path(), &removed.to_string_lossy());
    // Registered with launchd, failing every wake-up.
    let runner = MockRunner::new(vec![Ok(true), Ok(true), Ok(true)]);

    let convergence = converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Launchd,
        &runner,
        home.path(),
    )
    .expect("converge rewrites the stale unit");

    let ClockUnitConvergence::Rewritten(rewrite) = convergence else {
        panic!("expected a rewrite, got {convergence:?}");
    };
    assert_eq!(
        rewrite.drift,
        ClockUnitDrift::ProgramMissing {
            previous: removed.clone()
        }
    );
    assert_eq!(rewrite.unit_path, unit);
    assert!(rewrite.reactivated);
    assert!(rewrite.manual_steps.is_empty());
    let plist = fs::read_to_string(&unit).expect("rewritten plist");
    assert!(
        plist.contains(&running.display().to_string()),
        "rewritten unit must name the running binary: {plist}"
    );
    assert!(!plist.contains(&removed.display().to_string()), "{plist}");
    assert!(plist.contains("<string>clock</string>"), "{plist}");
    assert_eq!(
        runner.commands(),
        vec![
            "launchctl list com.orbit.sweep".to_string(),
            format!("launchctl unload {}", unit.display()),
            format!("launchctl load {}", unit.display()),
        ]
    );
}

/// systemd drift is the same failure with a different manager: the service
/// keeps naming a binary the installer moved away from.
#[test]
fn systemd_converge_rewrites_a_moved_program_and_rearms_the_timer() {
    let root = tempdir().expect("global root");
    let home = tempdir().expect("home");
    let running = home.path().join("orbit/bin/orbit");
    let moved_from = home.path().join("cargo/bin/orbit");
    for path in [&running, &moved_from] {
        fs::create_dir_all(path.parent().expect("parent")).expect("program parent");
        fs::write(path, "binary").expect("program");
    }
    let unit = write_systemd_unit(home.path(), &moved_from.to_string_lossy());
    let runner = MockRunner::new(vec![Ok(true), Ok(true), Ok(true)]);

    let convergence = converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .expect("converge rewrites the stale unit");

    let ClockUnitConvergence::Rewritten(rewrite) = convergence else {
        panic!("expected a rewrite, got {convergence:?}");
    };
    assert_eq!(
        rewrite.drift,
        ClockUnitDrift::ProgramMoved {
            previous: moved_from.clone()
        }
    );
    assert!(rewrite.reactivated);
    assert_eq!(
        rewrite.files_written,
        vec![unit.clone(), unit.with_file_name("orbit-sweep.timer")]
    );
    let service = fs::read_to_string(&unit).expect("rewritten service");
    assert!(
        service.contains(&format!("ExecStart={} clock tick", running.display())),
        "{service}"
    );
    assert!(
        unit.with_file_name("orbit-sweep.timer").exists(),
        "the timer is written alongside the service"
    );
    assert_eq!(
        runner.commands(),
        vec![
            "systemctl --user is-enabled orbit-sweep.timer",
            "systemctl --user daemon-reload",
            "systemctl --user restart orbit-sweep.timer",
        ]
    );
}

/// Repair must not resume a clock the operator paused: the file is corrected,
/// the manager is left alone.
#[test]
fn converge_rewrites_a_paused_unit_without_starting_it() {
    let root = tempdir().expect("global root");
    let home = tempdir().expect("home");
    let running = home.path().join("orbit");
    fs::write(&running, "binary").expect("running binary");
    let unit = write_launchd_unit(home.path(), "/opt/homebrew/bin/orbit");
    let runner = MockRunner::new(vec![Ok(false)]);

    let convergence = converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Launchd,
        &runner,
        home.path(),
    )
    .expect("converge rewrites the paused unit");

    let ClockUnitConvergence::Rewritten(rewrite) = convergence else {
        panic!("expected a rewrite, got {convergence:?}");
    };
    assert!(!rewrite.reactivated);
    assert!(
        rewrite.manual_steps.is_empty(),
        "a paused clock needs no follow-up: {:?}",
        rewrite.manual_steps
    );
    assert!(
        fs::read_to_string(&unit)
            .expect("rewritten plist")
            .contains(&running.display().to_string())
    );
    assert_eq!(runner.commands(), vec!["launchctl list com.orbit.sweep"]);
    assert!(
        !clock_reload_pending_path(root.path()).exists(),
        "a paused clock has no reload to retry"
    );
}

/// A rewritten unit the manager refuses to reload is unfinished work, not a
/// clean repair.
#[test]
fn converge_reports_manual_steps_when_the_manager_will_not_reload() {
    let root = tempdir().expect("global root");
    let home = tempdir().expect("home");
    let running = home.path().join("orbit");
    fs::write(&running, "binary").expect("running binary");
    let unit = write_launchd_unit(home.path(), "/opt/homebrew/bin/orbit");
    let runner = MockRunner::new(vec![Ok(true), Ok(true), Ok(false)]);

    let convergence = converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Launchd,
        &runner,
        home.path(),
    )
    .expect("converge rewrites the stale unit");

    assert!(convergence.needs_follow_up());
    assert_eq!(
        convergence.manual_steps(),
        [format!("launchctl load {}", unit.display())]
    );
    assert!(convergence.summary().contains("NOT reloaded"));
    assert!(
        clock_reload_pending_path(root.path()).exists(),
        "a failed reload is remembered so the next pass retries it"
    );
}

/// The recovery path after a failed reload is to re-run `orbit update` or
/// `orbit clock repair`. The rewritten file already names this binary, so the
/// retry must re-register the unit rather than report it current.
#[test]
fn launchd_converge_retries_the_reload_a_failed_repair_left_pending() {
    let root = tempdir().expect("global root");
    let home = tempdir().expect("home");
    let running = home.path().join("orbit");
    fs::write(&running, "binary").expect("running binary");
    let unit = write_launchd_unit(home.path(), "/opt/homebrew/bin/orbit");
    // Registered; the unload succeeds and the load fails, leaving the job
    // unloaded — indistinguishable from a paused clock to `launchctl list`.
    let first = MockRunner::new(vec![Ok(true), Ok(true), Ok(false)]);
    let convergence = converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Launchd,
        &first,
        home.path(),
    )
    .expect("first pass rewrites the unit");
    assert!(convergence.needs_follow_up());
    let rewritten = fs::read_to_string(&unit).expect("rewritten plist");

    let second = MockRunner::new(vec![Ok(true), Ok(true)]);
    let convergence = converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Launchd,
        &second,
        home.path(),
    )
    .expect("second pass retries the reload");

    let ClockUnitConvergence::Reloaded(reload) = &convergence else {
        panic!("expected a reload retry, got {convergence:?}");
    };
    assert_eq!(reload.unit_path, unit);
    assert_eq!(reload.program, running);
    assert!(reload.reactivated);
    assert!(reload.manual_steps.is_empty());
    assert!(!convergence.needs_follow_up());
    assert!(
        convergence.summary().contains("(reloaded)"),
        "{}",
        convergence.summary()
    );
    assert_eq!(
        second.commands(),
        vec![
            format!("launchctl unload {}", unit.display()),
            format!("launchctl load {}", unit.display()),
        ]
    );
    assert_eq!(
        fs::read_to_string(&unit).expect("plist"),
        rewritten,
        "the retry only talks to the manager"
    );
    assert!(
        !clock_reload_pending_path(root.path()).exists(),
        "a successful retry forgets the pending reload"
    );

    // Once re-registered the unit really is current: no more manager traffic.
    let third = MockRunner::new(Vec::new());
    let convergence = converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Launchd,
        &third,
        home.path(),
    )
    .expect("third pass inspects the current unit");
    assert!(matches!(
        convergence,
        ClockUnitConvergence::AlreadyCurrent { .. }
    ));
    assert!(third.commands().is_empty());
}

/// systemd has the same shape: a restart the manager refused is retried on
/// the next pass instead of being reported as current.
#[test]
fn systemd_converge_retries_the_restart_a_failed_repair_left_pending() {
    let root = tempdir().expect("global root");
    let home = tempdir().expect("home");
    let running = home.path().join("orbit/bin/orbit");
    let moved_from = home.path().join("cargo/bin/orbit");
    for path in [&running, &moved_from] {
        fs::create_dir_all(path.parent().expect("parent")).expect("program parent");
        fs::write(path, "binary").expect("program");
    }
    write_systemd_unit(home.path(), &moved_from.to_string_lossy());
    // Enabled; daemon-reload succeeds and the restart fails.
    let first = MockRunner::new(vec![Ok(true), Ok(true), Ok(false)]);
    let convergence = converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Systemd,
        &first,
        home.path(),
    )
    .expect("first pass rewrites the units");
    assert!(convergence.needs_follow_up());

    let second = MockRunner::new(vec![Ok(true), Ok(true)]);
    let convergence = converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Systemd,
        &second,
        home.path(),
    )
    .expect("second pass retries the restart");

    let ClockUnitConvergence::Reloaded(reload) = &convergence else {
        panic!("expected a reload retry, got {convergence:?}");
    };
    assert!(reload.reactivated);
    assert!(!convergence.needs_follow_up());
    assert_eq!(
        second.commands(),
        vec![
            "systemctl --user daemon-reload",
            "systemctl --user restart orbit-sweep.timer",
        ]
    );
    assert!(!clock_reload_pending_path(root.path()).exists());
}

/// A retry the manager refuses again is still unfinished work, and the next
/// pass keeps retrying.
#[test]
fn converge_keeps_reporting_follow_up_while_the_retry_keeps_failing() {
    let root = tempdir().expect("global root");
    let home = tempdir().expect("home");
    let running = home.path().join("orbit");
    fs::write(&running, "binary").expect("running binary");
    let unit = write_launchd_unit(home.path(), "/opt/homebrew/bin/orbit");
    let first = MockRunner::new(vec![Ok(true), Ok(true), Ok(false)]);
    converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Launchd,
        &first,
        home.path(),
    )
    .expect("first pass rewrites the unit");

    let second = MockRunner::new(vec![Ok(true), Ok(false)]);
    let convergence = converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Launchd,
        &second,
        home.path(),
    )
    .expect("second pass retries the reload");

    let ClockUnitConvergence::Reloaded(reload) = &convergence else {
        panic!("expected a reload retry, got {convergence:?}");
    };
    assert!(!reload.reactivated);
    assert!(convergence.needs_follow_up());
    assert_eq!(
        convergence.manual_steps(),
        [format!("launchctl load {}", unit.display())]
    );
    assert!(convergence.summary().contains("NOT reloaded"));
    assert!(
        clock_reload_pending_path(root.path()).exists(),
        "the reload stays pending until the manager accepts it"
    );
}

/// A binary that moves again before the retry runs is not a paused clock: the
/// pending reload says the operator wanted it registered, so the rewrite
/// re-arms it even though the manager reports it unloaded.
#[test]
fn converge_rearms_a_unit_that_drifted_again_after_a_failed_reload() {
    let root = tempdir().expect("global root");
    let home = tempdir().expect("home");
    let first_binary = home.path().join("first/orbit");
    let second_binary = home.path().join("second/orbit");
    for path in [&first_binary, &second_binary] {
        fs::create_dir_all(path.parent().expect("parent")).expect("program parent");
        fs::write(path, "binary").expect("program");
    }
    let unit = write_launchd_unit(home.path(), "/opt/homebrew/bin/orbit");
    let first = MockRunner::new(vec![Ok(true), Ok(true), Ok(false)]);
    converge_clock_unit_with(
        root.path(),
        &first_binary,
        ClockPlatform::Launchd,
        &first,
        home.path(),
    )
    .expect("first pass rewrites the unit");

    // No status probe is configured: the pending reload answers it.
    let second = MockRunner::new(vec![Ok(true), Ok(true)]);
    let convergence = converge_clock_unit_with(
        root.path(),
        &second_binary,
        ClockPlatform::Launchd,
        &second,
        home.path(),
    )
    .expect("second pass rewrites the unit again");

    let ClockUnitConvergence::Rewritten(rewrite) = &convergence else {
        panic!("expected a rewrite, got {convergence:?}");
    };
    assert_eq!(
        rewrite.drift,
        ClockUnitDrift::ProgramMoved {
            previous: first_binary.clone()
        }
    );
    assert!(rewrite.reactivated);
    assert!(rewrite.manual_steps.is_empty());
    assert_eq!(
        second.commands(),
        vec![
            format!("launchctl unload {}", unit.display()),
            format!("launchctl load {}", unit.display()),
        ]
    );
    assert!(
        fs::read_to_string(&unit)
            .expect("rewritten plist")
            .contains(&second_binary.display().to_string())
    );
    assert!(!clock_reload_pending_path(root.path()).exists());
}

/// A unit that runs this binary through the compatibility alias is converged
/// to `orbit clock tick` without the operator reaching for another command.
#[test]
fn converge_rewrites_a_legacy_sweep_invocation() {
    let root = tempdir().expect("global root");
    let home = tempdir().expect("home");
    let running = home.path().join("orbit");
    fs::write(&running, "binary").expect("running binary");
    let unit_dir = home.path().join(".config/systemd/user");
    fs::create_dir_all(&unit_dir).expect("systemd user dir");
    let unit = unit_dir.join("orbit-sweep.service");
    fs::write(
        &unit,
        format!(
            "[Service]\nType=oneshot\nExecStart={} sweep\n",
            running.display()
        ),
    )
    .expect("write legacy service");
    let runner = MockRunner::new(vec![Ok(true), Ok(true), Ok(true)]);

    let convergence = converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .expect("converge rewrites the legacy invocation");

    let ClockUnitConvergence::Rewritten(rewrite) = convergence else {
        panic!("expected a rewrite, got {convergence:?}");
    };
    assert_eq!(rewrite.drift, ClockUnitDrift::InvocationStale);
    assert!(
        fs::read_to_string(&unit)
            .expect("rewritten service")
            .contains("clock tick")
    );
}

#[test]
fn converge_leaves_a_current_unit_and_its_manager_alone() {
    let root = tempdir().expect("global root");
    let home = tempdir().expect("home");
    let running = home.path().join("orbit");
    fs::write(&running, "binary").expect("running binary");
    let unit = write_launchd_unit(home.path(), &running.to_string_lossy());
    let before = fs::read_to_string(&unit).expect("installed plist");
    let runner = MockRunner::new(Vec::new());

    let convergence = converge_clock_unit_with(
        root.path(),
        &running,
        ClockPlatform::Launchd,
        &runner,
        home.path(),
    )
    .expect("converge inspects the current unit");

    assert_eq!(
        convergence,
        ClockUnitConvergence::AlreadyCurrent {
            unit_path: unit.clone(),
            program: running.clone(),
        }
    );
    assert_eq!(fs::read_to_string(&unit).expect("plist"), before);
    assert!(runner.commands().is_empty());
    assert!(!convergence.needs_follow_up());
}

#[test]
fn converge_without_an_installed_unit_changes_nothing() {
    let root = tempdir().expect("global root");
    let home = tempdir().expect("home");
    let runner = MockRunner::new(Vec::new());

    let convergence = converge_clock_unit_with(
        root.path(),
        Path::new("/opt/orbit/bin/orbit"),
        ClockPlatform::Systemd,
        &runner,
        home.path(),
    )
    .expect("converge is a no-op without a unit");

    assert_eq!(convergence, ClockUnitConvergence::NoUnitInstalled);
    assert!(runner.commands().is_empty());
    assert!(!home.path().join(".config/systemd/user").exists());
}

/// `orbit sweep` run by hand from another install is the one moment the drift
/// is observable, so it says so on stderr.
#[test]
fn a_pass_from_another_binary_warns_and_names_the_repair() {
    let home = tempdir().expect("home");
    let running = home.path().join("cargo/orbit");
    let unit_program = home.path().join("homebrew/orbit");
    for path in [&running, &unit_program] {
        fs::create_dir_all(path.parent().expect("parent")).expect("program parent");
        fs::write(path, "binary").expect("program");
    }
    let unit = write_launchd_unit(home.path(), &unit_program.to_string_lossy());

    let warning = clock_unit_drift_warning_at(home.path(), ClockPlatform::Launchd, &running)
        .expect("a pass from another binary warns");

    assert!(warning.contains(&unit.display().to_string()), "{warning}");
    assert!(
        warning.contains(&unit_program.display().to_string()),
        "{warning}"
    );
    assert!(
        warning.contains(&running.display().to_string()),
        "{warning}"
    );
    assert!(warning.contains("orbit clock repair"), "{warning}");
    assert_eq!(warning.lines().count(), 1, "{warning}");
}

#[test]
fn a_pass_from_the_binary_the_unit_names_is_silent() {
    let home = tempdir().expect("home");
    let running = home.path().join("orbit");
    fs::write(&running, "binary").expect("running binary");
    write_launchd_unit(home.path(), &running.to_string_lossy());

    assert_eq!(
        clock_unit_drift_warning_at(home.path(), ClockPlatform::Launchd, &running),
        None
    );
}
