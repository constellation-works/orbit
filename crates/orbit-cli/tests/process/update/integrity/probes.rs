//! Probe lifetimes at the signed-release update boundary. Reuse the integrity
//! fixture without adding another responsibility to its large parent module.

use super::*;
use orbit_common::process::identity::{ProcessLiveness, probe_process_liveness};

#[test]
fn version_probe_timeout_tears_down_descendants_before_and_after_parent_exit() {
    assert_probe_descendants(false);
}

#[test]
fn contract_probe_timeout_tears_down_descendants_before_and_after_parent_exit() {
    assert_probe_descendants(true);
}

#[cfg(target_os = "linux")]
#[test]
fn version_probe_retries_busy_executables_and_preserves_the_spawn_error() {
    let root = tempfile::tempdir().expect("probe root");
    let executable = root.path().join("candidate");
    fs::write(&executable, "#!/bin/sh\necho 'orbit 99.0.0'\n").expect("candidate");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).expect("executable");
    let writer = OpenOptions::new()
        .write(true)
        .open(&executable)
        .expect("writer");
    // A held write descriptor deterministically makes every exec fail with
    // ETXTBSY. The retry window must be exhausted rather than refusing the
    // first attempt, and the eventual error must remain the OS spawn error.
    let started = Instant::now();
    let error = orbit_cmd::update::converge::probe_version(&executable).expect_err("busy");
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "busy retry window was skipped"
    );
    assert!(
        error
            .to_string()
            .contains(&format!("os error {}", libc::ETXTBSY)),
        "{error}"
    );
    drop(writer);
    assert_eq!(
        orbit_cmd::update::converge::probe_version(&executable).expect("released executable"),
        "99.0.0"
    );
}

fn assert_probe_descendants(contract: bool) {
    for parent_exits in [false, true] {
        let install = Install::new(None);
        let pid_file = install._root.path().join("descendant.pid");
        // Also clean up on assertion failure against a regressed runner.
        let _descendant = DescendantGuard(&pid_file);
        let condition = if contract {
            "[ \"$1\" = update ] && [ \"$2\" = --contract ]"
        } else {
            "[ \"$1\" = --version ]"
        };
        let reply = if contract {
            "echo '{\"schema_version\":1,\"contract\":\"executable-generation-v1\"}'"
        } else {
            "echo 'orbit 99.0.0'"
        };
        let finish = if parent_exits { "exit 0" } else { "wait" };
        let script = format!(
            "#!/bin/sh\n\
             if {condition}; then\n\
               sleep 600 &\n\
               echo $! > '{}'\n\
               {reply}\n\
               {finish}\n\
             fi\n\
             if [ \"$1\" = --version ]; then echo 'orbit 99.0.0'; exit 0; fi\n\
             exit 1\n",
            pid_file.display()
        );
        install.publish("99.0.0", Some(&tar_gz(script.as_bytes())), true);
        let mut command = install.command();
        command
            .env(PROBE_TIMEOUT_ENV, HUNG_PROBE_BOUND_SECS.to_string())
            .arg("update");
        let output = bounded_output(&mut command, "update with a descendant holding probe pipes");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{stderr}");
        assert!(
            stderr.contains(&format!("did not finish within {HUNG_PROBE_BOUND_SECS}s")),
            "contract={contract}, parent_exits={parent_exits}: {stderr}"
        );
        assert!(
            stderr.contains(if contract {
                "candidate admission contract unavailable"
            } else {
                "--version"
            }),
            "the intended probe must cause the refusal: {stderr}"
        );
        let pid = fs::read_to_string(&pid_file)
            .expect("probe actually spawned its descendant")
            .trim()
            .parse::<u32>()
            .expect("descendant pid");
        test_env::wait_until("the probe's descendant to stop", || {
            probe_process_liveness(pid, None) == ProcessLiveness::Exited
        });
        install.assert_untouched("probe with an inherited pipe");
        install.assert_no_backup("probe with an inherited pipe");
    }
}

struct DescendantGuard<'a>(&'a Path);

impl Drop for DescendantGuard<'_> {
    fn drop(&mut self) {
        let pid = fs::read_to_string(self.0)
            .ok()
            .and_then(|pid| pid.trim().parse::<u32>().ok());
        if let Some(pid) = pid
            .filter(|pid| *pid > 1 && probe_process_liveness(*pid, None) == ProcessLiveness::Alive)
        {
            // Safety: this PID was written by our disposable probe. The
            // signal cleans up its sleeping descendant on failed checks.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
    }
}
