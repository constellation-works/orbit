//! Deterministic deadline faults at the probe boundary, where inherited pipes
//! must finish even after the probe leader exits (test-strategy fault injection).

use std::cell::Cell;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use orbit_common::process::identity::{ProcessLiveness, probe_process_liveness};
use orbit_common::test_env::{wait_until, with_supervision_clock};

use super::super::converge::{probe_version, require_admission_contract};

#[test]
fn probe_timeouts_tear_down_descendants_before_and_after_parent_exit() {
    for contract in [false, true] {
        for parent_exits in [false, true] {
            let root = tempfile::tempdir().expect("probe fixture");
            let executable = root.path().join("candidate");
            let ready = root.path().join("ready");
            let pid_file = root.path().join("descendant.pid");
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
            fs::write(&executable, format!(
                "#!/bin/sh\nif {condition}; then\n sleep 600 &\n echo $! > '{}'\n {reply}\n : > '{}'\n {finish}\nfi\nexit 1\n",
                pid_file.display(), ready.display(),
            )).expect("candidate script");
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
                .expect("executable");
            let origin = Instant::now();
            let initialized = Cell::new(false);
            // The first clock read starts supervision. Expire only once the
            // intended probe has actually spawned its pipe-holding helper.
            let error = with_supervision_clock(
                move || {
                    if initialized.replace(true) && ready.exists() {
                        origin + Duration::from_secs(3_601)
                    } else {
                        origin
                    }
                },
                || {
                    if contract {
                        require_admission_contract(&executable)
                            .err()
                            .expect("contract must time out")
                    } else {
                        probe_version(&executable).expect_err("version must time out")
                    }
                },
            );
            let message = error.to_string();
            assert!(message.contains("did not finish within"), "{message}");
            assert!(
                message.contains(if contract {
                    "candidate admission contract unavailable"
                } else {
                    "--version"
                }),
                "the intended probe must cause the refusal: {message}"
            );
            let pid = fs::read_to_string(&pid_file)
                .expect("probe actually spawned its descendant")
                .trim()
                .parse::<u32>()
                .expect("descendant pid");
            wait_until("the probe's descendant to stop", || {
                probe_process_liveness(pid, None) == ProcessLiveness::Exited
            });
        }
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
            // SAFETY: only this disposable probe writes the helper PID.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
    }
}
