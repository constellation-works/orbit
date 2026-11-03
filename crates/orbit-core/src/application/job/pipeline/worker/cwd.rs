//! End detached workers whose working directory has been unlinked. This
//! watches the process's actual cwd, including a deleted inode at a path that
//! has since been recreated, without querying or recreating removed stores.

use std::io::ErrorKind;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use orbit_common::OrbitError;

const CWD_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Scoped to the entire worker entrypoint, including admission waits, adopted
/// runs and blocking steps. Drop wakes and joins the monitor on ordinary exits.
pub(super) struct WorkerCwdGuard {
    stop: Option<mpsc::SyncSender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl WorkerCwdGuard {
    pub(super) fn start(run_id: &str) -> Result<Self, OrbitError> {
        let run_id = run_id.to_owned();
        let (stop, stopped) = mpsc::sync_channel::<()>(1);
        let thread = thread::Builder::new()
            .name("orbit-worker-cwd".into())
            .spawn(move || {
                loop {
                    // Permission and transient I/O errors do not establish
                    // deletion. A rename alone also leaves getcwd usable.
                    if let Err(error) = std::env::current_dir()
                        && error.kind() == ErrorKind::NotFound
                    {
                        tracing::error!(
                            run_id,
                            %error,
                            "pipeline worker working directory was deleted; terminating worker",
                        );
                        terminate_worker_with_deleted_cwd();
                        return;
                    }
                    match stopped.recv_timeout(CWD_POLL_INTERVAL) {
                        Err(RecvTimeoutError::Timeout) => {}
                        _ => return,
                    }
                }
            })
            .map_err(|error| OrbitError::Execution(format!("start worker cwd monitor: {error}")))?;
        Ok(Self {
            stop: Some(stop),
            thread: Some(thread),
        })
    }
}

impl Drop for WorkerCwdGuard {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn terminate_worker_with_deleted_cwd() {
    #[cfg(unix)]
    {
        // Signal the whole process, not just this monitor thread. The existing
        // supervised-child signal guard forwards TERM and reaps its children
        // before re-raising it. Bound shutdown if another handler stalls.
        // SAFETY: this process is still executing, so its own PID cannot be
        // reused; neither signal can reach an unrelated process.
        unsafe { libc::kill(std::process::id() as libc::pid_t, libc::SIGTERM) };
        thread::sleep(Duration::from_secs(5));
        // SAFETY: same live process identity as above.
        unsafe { libc::kill(std::process::id() as libc::pid_t, libc::SIGKILL) };
    }
    #[cfg(not(unix))]
    std::process::exit(1);
}
