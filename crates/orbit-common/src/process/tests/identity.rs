#[cfg(target_os = "macos")]
mod liveness {
    use crate::process::identity::*;

    // macOS: native libproc liveness must classify an unreaped zombie and its group as exited.
    #[cfg(target_os = "macos")]
    #[test]
    fn darwin_native_probes_classify_an_unreaped_zombie_and_its_group_as_exited() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        struct ReapingChild(std::process::Child);

        impl Drop for ReapingChild {
            fn drop(&mut self) {
                if self.0.try_wait().ok().flatten().is_none() {
                    let _ = self.0.kill();
                }
                let _ = self.0.wait();
            }
        }

        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("exit 0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Safety: the child has not spawned yet; setsid isolates the process
        // group queried by this test from the test runner's group.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = ReapingChild(command.spawn().expect("spawn isolated child"));
        let pid = child.0.id();
        let deadline = Instant::now() + Duration::from_secs(3);
        while process_is_alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }

        assert!(
            !process_is_alive(pid),
            "unreaped Darwin zombie must not count as live"
        );
        assert_eq!(
            probe_process_group_liveness(pid as libc::pid_t),
            KernelLiveness::Exited,
            "a zombie-only Darwin group must be stopped"
        );
        child.0.wait().expect("reap isolated zombie");
    }
}

// macOS: libproc start-time rendering must match persisted ps owner identities exactly.
#[cfg(target_os = "macos")]
mod darwin {
    use std::io;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    use crate::process::identity::darwin_lstart_utc;
    use crate::test_env::{PsRun, classify_ps, ps_lstart_utc, report_ps_denied};

    #[derive(Debug, PartialEq)]
    enum PsComparison {
        Matches,
        Skipped(String),
    }

    /// Compare a `ps -o lstart=` run with the libproc rendering. A sandbox
    /// refusal to start `ps` skips; a failed `ps` or different output fails.
    fn compare_with_ps(ps: PsRun, libproc: Option<&str>) -> Result<PsComparison, String> {
        let output = match ps {
            PsRun::Ran(output) => output,
            PsRun::Denied(reason) => return Ok(PsComparison::Skipped(reason)),
        };
        if !output.status.success() {
            return Err(format!(
                "ps failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let from_ps = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if libproc == Some(from_ps.as_str()) {
            Ok(PsComparison::Matches)
        } else {
            Err(format!(
                "libproc rendered {libproc:?}, ps printed {from_ps:?}"
            ))
        }
    }

    /// The sandbox-safe probe must produce exactly what `ps` prints, or a
    /// token a worker computes would never match the one the host recorded.
    /// A managed executor's sandbox refuses to exec `ps`; the deferral notice
    /// says why nothing was compared, and a claim host replays it [ORB-15587].
    #[test]
    fn libproc_start_time_renders_exactly_as_ps_lstart() {
        let pid = std::process::id();
        match compare_with_ps(ps_lstart_utc(pid), darwin_lstart_utc(pid).as_deref()) {
            Ok(PsComparison::Matches) => {}
            Ok(PsComparison::Skipped(reason)) => report_ps_denied(
                "process::tests::identity::darwin::libproc_start_time_renders_exactly_as_ps_lstart",
                &reason,
            ),
            Err(failure) => panic!("{failure}"),
        }
    }

    /// A sandbox denial must not hide a real rendering mismatch: everything
    /// except a `PermissionDenied` spawn still fails the comparison.
    #[test]
    fn only_a_denied_ps_spawn_skips_the_comparison() {
        let lstart = "Wed Oct  7 01:02:03 2026";
        let ran = |code: i32, stdout: &str| {
            classify_ps(Ok(Output {
                status: ExitStatus::from_raw(code << 8),
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
            }))
        };

        assert_eq!(
            compare_with_ps(ran(0, &format!("{lstart}\n")), Some(lstart)),
            Ok(PsComparison::Matches)
        );
        for (case, ps, libproc) in [
            (
                "different output",
                ran(0, "Wed Oct  7 01:02:04 2026\n"),
                Some(lstart),
            ),
            ("no libproc rendering", ran(0, lstart), None),
            ("ps ran and failed", ran(1, ""), Some(lstart)),
        ] {
            assert!(compare_with_ps(ps, libproc).is_err(), "{case} must fail");
        }
        let denied = classify_ps(Err(io::Error::from_raw_os_error(libc::EPERM)));
        assert!(matches!(
            compare_with_ps(denied, Some(lstart)),
            Ok(PsComparison::Skipped(_))
        ));
        assert!(
            std::panic::catch_unwind(|| classify_ps(Err(io::ErrorKind::NotFound.into()))).is_err(),
            "a missing `ps` is a failure, not a skip"
        );
    }
}

// Criterion 2: partial /proc failures and disappearance cannot be forced at the
// public probe reliably without changing host permissions or racing a process.
#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::OsString;
    use std::io::{self, ErrorKind};

    use crate::process::identity::{KernelLiveness, linux_process_group_liveness_with};

    #[derive(Clone, Copy)]
    enum Stat {
        Member(char, i32),
        Error(ErrorKind),
        Malformed(&'static str),
    }

    #[test]
    fn partial_linux_group_scans_cannot_certify_death_and_vanished_pids_do_not_delay_cleanup() {
        use KernelLiveness::{Alive, Exited, Unknown};
        use Stat::{Error, Malformed, Member};

        let zombie = Member('Z', 100);
        let live = Member('S', 100);
        type Entries = Result<Vec<Result<&'static str, ErrorKind>>, ErrorKind>;
        let entries = || Ok(vec![Ok("100"), Ok("101")]);
        let cases: Vec<(&str, Entries, Vec<Stat>, Option<KernelLiveness>)> = vec![
            (
                "unreadable member after zombie",
                entries(),
                vec![zombie, Error(ErrorKind::PermissionDenied)],
                Some(Unknown),
            ),
            (
                "I/O failure after zombie",
                entries(),
                vec![zombie, Error(ErrorKind::Other)],
                Some(Unknown),
            ),
            (
                "malformed stat after zombie",
                entries(),
                vec![zombie, Malformed("unparseable")],
                Some(Unknown),
            ),
            (
                "invalid stat fields after zombie",
                entries(),
                vec![zombie, Malformed("101 (child) S invalid")],
                Some(Unknown),
            ),
            (
                "vanished member after zombie",
                entries(),
                vec![zombie, Error(ErrorKind::NotFound)],
                Some(Exited),
            ),
            (
                "complete zombie-only scan",
                entries(),
                vec![zombie, zombie],
                Some(Exited),
            ),
            (
                "live member after zombie",
                entries(),
                vec![zombie, live],
                Some(Alive),
            ),
            (
                "live member after unreadable PID",
                entries(),
                vec![Error(ErrorKind::PermissionDenied), live],
                Some(Alive),
            ),
            (
                "other group cannot mask uncertainty",
                entries(),
                vec![Member('S', 200), Error(ErrorKind::Other)],
                Some(Unknown),
            ),
            (
                "other group ignored",
                entries(),
                vec![zombie, Member('S', 200)],
                Some(Exited),
            ),
            (
                "directory entry failure after zombie",
                Ok(vec![Ok("100"), Err(ErrorKind::Other)]),
                vec![zombie],
                Some(Unknown),
            ),
            (
                "live member after directory entry failure",
                Ok(vec![Err(ErrorKind::Other), Ok("101")]),
                vec![zombie, live],
                Some(Alive),
            ),
            (
                "non-PID entries ignored",
                Ok(vec![Ok("self"), Ok("100")]),
                vec![zombie],
                Some(Exited),
            ),
            (
                "only vanished PIDs use signal fallback",
                entries(),
                vec![Error(ErrorKind::NotFound); 2],
                None,
            ),
            ("empty scan uses signal fallback", Ok(vec![]), vec![], None),
            (
                "unavailable proc uses signal fallback",
                Err(ErrorKind::PermissionDenied),
                vec![],
                None,
            ),
        ];
        for (case, entries, stats, expected) in cases {
            let entries = entries
                .map(|entries| {
                    entries
                        .into_iter()
                        .map(|entry| entry.map(OsString::from).map_err(io::Error::from))
                })
                .map_err(io::Error::from);
            let actual = linux_process_group_liveness_with(100, entries, |pid| {
                match stats[(pid - 100) as usize] {
                    Member(state, group) => Ok(format!(
                        "{pid} (child) {state} 1 {group} {}7",
                        "0 ".repeat(16)
                    )),
                    Error(kind) => Err(kind.into()),
                    Malformed(text) => Ok(text.to_owned()),
                }
            });
            assert_eq!(
                actual, expected,
                "{case}: partial observations must not certify group death; cleanup may skip SIGKILL only on Exited"
            );
        }
    }
}
