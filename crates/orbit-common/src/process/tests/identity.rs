#[cfg(unix)]
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
    use crate::test_env::{PsRun, classify_ps, ps_lstart_utc};

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
    #[test]
    // A managed executor's sandbox refuses to exec `ps`; the skip notice goes
    // to stderr so the run says why nothing was compared.
    #[allow(clippy::print_stderr)]
    fn libproc_start_time_renders_exactly_as_ps_lstart() {
        let pid = std::process::id();
        match compare_with_ps(ps_lstart_utc(pid), darwin_lstart_utc(pid).as_deref()) {
            Ok(PsComparison::Matches) => {}
            Ok(PsComparison::Skipped(reason)) => eprintln!("SKIP: {reason}"),
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
