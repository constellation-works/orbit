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
    use crate::process::identity::darwin_lstart_utc;

    /// The sandbox-safe probe must produce exactly what `ps` prints, or a
    /// token a worker computes would never match the one the host recorded.
    #[test]
    fn libproc_start_time_renders_exactly_as_ps_lstart() {
        let pid = std::process::id();
        let output = std::process::Command::new("ps")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .env("TZ", "UTC")
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .output()
            .expect("run ps");
        let from_ps = String::from_utf8_lossy(&output.stdout).trim().to_string();
        assert_eq!(darwin_lstart_utc(pid).as_deref(), Some(from_ps.as_str()));
    }
}
