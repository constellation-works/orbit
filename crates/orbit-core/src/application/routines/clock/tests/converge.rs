use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use orbit_common::OrbitError;

use super::super::converge::{
    ClockUnitConvergence, ClockUnitDrift, clock_reload_pending_path, converge_clock_unit_with,
};
use super::super::install::{write_launchd_unit, write_systemd_units};
use super::super::manager::{ClockPlatform, ManagerCommandOutput};
use super::super::program::discover_clock_unit_program;
use super::super::settings::ClockSettings;
use super::support::MockRunner;

/// An unqueryable manager is not a paused clock, and a recognized pause still is.
///
/// Guards the convergence path that used to treat every non-zero manager
/// status as paused: the unit file was rewritten, activation was skipped, and
/// the next repair reported the dead clock current. A bus failure shares
/// systemd's "No such file or directory" text with a missing unit, so the
/// transport diagnostic has to win.
#[test]
fn unqueryable_manager_leaves_follow_up_and_recognized_pause_does_not() {
    let cases = [
        Case {
            platform: ClockPlatform::Systemd,
            probes: vec![failed(
                "",
                "Failed to connect to bus: No such file or directory\n",
            )],
            runs: vec![Ok(false)],
            follow_up: true,
        },
        Case {
            platform: ClockPlatform::Systemd,
            probes: vec![failed("disabled\n", "")],
            runs: vec![Ok(false)],
            follow_up: false,
        },
        Case {
            platform: ClockPlatform::Launchd,
            probes: vec![failed("", ""), failed("", "")],
            runs: vec![Ok(false), Ok(false)],
            follow_up: true,
        },
        Case {
            platform: ClockPlatform::Launchd,
            probes: vec![failed(
                "",
                "Could not find service \"com.orbit.sweep\" in domain\n",
            )],
            runs: vec![Ok(false), Ok(false)],
            follow_up: false,
        },
    ];

    for case in cases {
        let fixture = DriftFixture::new(case.platform);
        let runner = MockRunner::with_probes(case.probes, case.runs);
        let convergence = converge_clock_unit_with(
            fixture.root.path(),
            &fixture.current,
            case.platform,
            &runner,
            fixture.home.path(),
        )
        .expect("classify the manager probe");

        let ClockUnitConvergence::Rewritten(rewrite) = &convergence else {
            panic!(
                "{:?}: drifted unit was not rewritten: {convergence:?}",
                case.platform
            );
        };
        assert!(
            matches!(rewrite.drift, ClockUnitDrift::ProgramMissing { .. }),
            "{:?}: installed program was not the missing binary",
            case.platform
        );
        assert!(
            !rewrite.reactivated,
            "{:?}: activation must fail closed in this fixture",
            case.platform
        );
        let installed = discover_clock_unit_program(fixture.home.path(), case.platform)
            .expect("unit stays installed")
            .expect("rewritten unit names a program");
        assert_eq!(
            installed.1, fixture.current,
            "{:?}: rewritten unit must name the running binary",
            case.platform
        );
        let marker = clock_reload_pending_path(fixture.root.path()).exists();
        assert_eq!(
            convergence.needs_follow_up(),
            case.follow_up,
            "{:?}: an unqueryable manager must not exit as a clean pause; a named disabled or not-loaded unit must stay paused",
            case.platform
        );
        assert_eq!(
            marker, case.follow_up,
            "{:?}: failed activation after an unqueryable manager must leave the reload marker",
            case.platform
        );
        assert_eq!(
            rewrite.manual_steps.is_empty(),
            !case.follow_up,
            "{:?}: manual steps follow a failed activation only",
            case.platform
        );
    }
}

struct Case {
    platform: ClockPlatform,
    probes: Vec<Result<ManagerCommandOutput, OrbitError>>,
    runs: Vec<Result<bool, OrbitError>>,
    follow_up: bool,
}

fn failed(stdout: &str, stderr: &str) -> Result<ManagerCommandOutput, OrbitError> {
    Ok(ManagerCommandOutput {
        success: false,
        exit_code: Some(1),
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    })
}

struct DriftFixture {
    root: TempDir,
    home: TempDir,
    current: PathBuf,
}

impl DriftFixture {
    fn new(platform: ClockPlatform) -> Self {
        let root = tempfile::tempdir().expect("global root");
        let home = tempfile::tempdir().expect("home");
        let stale = home.path().join("missing-orbit-bin");
        let current = root.path().join("current-orbit-bin");
        fs::write(&current, b"orbit").expect("write running binary");
        match platform {
            ClockPlatform::Systemd => {
                write_systemd_units(path_text(&stale), ClockSettings::default(), home.path())
                    .expect("write stale systemd unit");
            }
            ClockPlatform::Launchd => {
                write_launchd_unit(
                    root.path(),
                    path_text(&stale),
                    ClockSettings::default(),
                    home.path(),
                )
                .expect("write stale launchd unit");
            }
        }
        Self {
            root,
            home,
            current,
        }
    }
}

fn path_text(path: &Path) -> &str {
    path.to_str().expect("utf-8 temp path")
}
