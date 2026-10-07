//! Combinations of systemd recovery settings in installed units.

use super::super::inspect::{ClockUnitVerdict, RunningBinary, inspect_clock_unit_at};
use super::super::manager::ClockPlatform;

#[test]
fn inspection_rejects_missing_unbounded_and_parent_only_recovery_settings() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".config/systemd/user");
    std::fs::create_dir_all(&dir).unwrap();
    let program = home.path().join("orbit");
    std::fs::write(&program, "fixture").unwrap();
    let running = RunningBinary {
        path: program.clone(),
        version: "1.0.0".into(),
    };
    for (settings, safe) in [
        ("", false),
        ("TimeoutStartSec=infinity\nKillMode=mixed\n", false),
        ("TimeoutStartSec=0\nKillMode=mixed\n", false),
        ("TimeoutStartSec=600\nKillMode=process\n", false),
        ("TimeoutStartSec=10min\nKillMode=mixed\n", true),
        ("TimeoutStartSec=1min 30s\nKillMode=mixed\n", true),
        (
            "TimeoutStartSec=600\nKillMode=mixed\nTimeoutStartSec=\n",
            false,
        ),
    ] {
        std::fs::write(
            dir.join("orbit-sweep.service"),
            format!(
                "[Service]\n{settings}ExecStart={} clock tick\n",
                program.display()
            ),
        )
        .unwrap();
        let inspected =
            inspect_clock_unit_at(home.path(), ClockPlatform::Systemd, &running, |_| {
                Ok("1.0.0".into())
            });
        if safe {
            assert_eq!(inspected.verdict, ClockUnitVerdict::Matching, "{settings}");
        } else {
            assert!(
                matches!(inspected.verdict, ClockUnitVerdict::SafetyMismatch { .. }),
                "{settings}: {:?}",
                inspected.verdict
            );
            assert!(inspected.doctor_remediation().is_some());
        }
    }
}
