//! The host-signal probe reads logind's schedule file [ORB-12968]. These
//! tests supply read results directly, never accessing the real `/run`.

use std::io;

use chrono::{TimeZone, Utc};

use crate::runtime::host_signal::{
    FixedHostSignals, HostSignalProbe, SYSTEMD_SCHEDULED_SHUTDOWN_PATH, default_host_signal_probe,
    parse_systemd_schedule, scheduled_shutdown_from_read,
};

/// 2026-09-25T04:00:00Z in microseconds since the epoch, as `shutdown -r
/// 04:00` records it.
const REBOOT_AT_USEC: &str = "1790308800000000";

#[test]
fn a_logind_schedule_file_reports_its_mode_and_time() {
    let shutdown = scheduled_shutdown_from_read(Ok(format!(
        "USEC={REBOOT_AT_USEC}\nWARN_WALL=1\nMODE=reboot\nUID=0\n"
    )))
    .expect("a scheduled reboot");
    assert_eq!(shutdown.mode, "reboot");
    assert_eq!(shutdown.source, SYSTEMD_SCHEDULED_SHUTDOWN_PATH);
    assert_eq!(
        shutdown.scheduled_at,
        Utc.with_ymd_and_hms(2026, 9, 25, 4, 0, 0)
            .single()
            .expect("time")
    );
    assert_eq!(
        shutdown.describe(),
        "host reboot scheduled for 2026-09-25T04:00:00Z"
    );
}

#[test]
fn no_schedule_file_means_nothing_is_scheduled() {
    assert!(scheduled_shutdown_from_read(Err(io::ErrorKind::NotFound.into())).is_none());
}

#[test]
fn unreadable_schedule_file_holds_nothing() {
    assert!(scheduled_shutdown_from_read(Err(io::ErrorKind::PermissionDenied.into())).is_none());
}

#[test]
fn a_dry_run_or_malformed_schedule_holds_nothing() {
    for contents in [
        format!("USEC={REBOOT_AT_USEC}\nMODE=dry-reboot\n"),
        "MODE=poweroff\n".to_string(),
        "USEC=not-a-number\nMODE=poweroff\n".to_string(),
        format!("USEC={REBOOT_AT_USEC}\n"),
        String::new(),
    ] {
        assert!(scheduled_shutdown_from_read(Ok(contents)).is_none());
    }
}

#[test]
fn a_fixed_probe_reports_exactly_what_it_was_given() {
    assert!(FixedHostSignals::none().scheduled_shutdown().is_none());
    let shutdown = parse_systemd_schedule(
        &format!("USEC={REBOOT_AT_USEC}\nMODE=poweroff\n"),
        "fixture",
    )
    .expect("fixture schedule");
    assert_eq!(
        FixedHostSignals::scheduled(shutdown.clone()).scheduled_shutdown(),
        Some(shutdown)
    );
}

/// Unit tests must not depend on whether the machine running them has a
/// reboot pending, so the default probe in a test build reports nothing.
#[test]
fn the_default_probe_in_a_test_build_never_reads_the_host() {
    assert!(default_host_signal_probe().scheduled_shutdown().is_none());
}
