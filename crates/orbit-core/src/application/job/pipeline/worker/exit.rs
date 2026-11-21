//! How a pipeline worker process's exit is observed, written and amended
//! [ORB-17179].
//!
//! The persisted exit-record format and its readers live with the run
//! (`job/run/worker_exit/`), which reconciliation reads. This module is the
//! writing side. Only the worker's parent, the supervisor that launched it,
//! can reap its exit status. When the supervisor is gone, init or the user
//! manager reaps the worker and its status is lost. So:
//!
//! - The supervisor records a `pipeline.worker.exited` audit row the moment
//!   it reaps, before it settles the run: exit code or terminating signal,
//!   and the worker scope's OOM-kill and refused-fork counters while the
//!   scope's cgroup still exists ([`WorkerExit::observe`]).
//!   It first saves the reap to the already-open worker log, so a failed
//!   audit insert cannot erase that observation. The worker also logs its
//!   own entry and application return independently of the store; the
//!   run's `worker_exit::lifecycle` reader recovers these diagnostic receipts
//!   when the submitting process or its audit is unavailable.
//! - A supervisor that reaps after reconciliation already wrote "no exit
//!   status was observed" appends what it observed to that diagnostic
//!   ([`note_exit_after_reconciliation`]). Reconciliation that settles before
//!   the row exists still writes `interrupted`; the amendment names the sender
//!   and does not change that terminal state.
//! - Waiting reveals no sender. So on Linux the worker catches its own
//!   terminating signals for its lifetime, writes the sender another process
//!   gave the kernel to its log, and dies from the same signal. The
//!   supervisor reads that line from the log before it records the exit
//!   [ORB-18166]. With no such line the exit says the sender is unknown.

use std::process::ExitStatus;
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::{JobRunStepParams, JobRunStoreBackend};
use orbit_types::workflow::JobRunState;

use crate::application::job::run::WORKER_TERMINATED_ERROR_CODE;
use crate::application::job::run::worker_exit::{
    NO_EXIT_OBSERVED, ScopeEvidence, SignalSender, WorkerExit,
};
use crate::application::job::run::worker_scope::WorkerScopeCgroup;

/// Marks a diagnostic a supervisor that reaped after reconciliation amended.
const OBSERVED_AFTERWARDS: &str = "afterwards its supervisor reaped";
/// How long a supervisor that lost the race waits for reconciliation's
/// diagnostic step, which is written just after the interrupted state.
const RECONCILED_STEP_WAIT: Duration = Duration::from_secs(5);

impl SignalSender {
    /// The sender the worker `worker_pid` wrote to its log, if its last such
    /// line names that worker and `signal`.
    fn from_worker_log(log_tail: &str, worker_pid: u32, signal: i32) -> Option<Self> {
        let line = log_tail
            .lines()
            .rev()
            .find_map(|line| line.trim().strip_prefix(SIGNAL_SENDER_MARKER))?;
        let (fields, command) = line.split_once(" sender_comm=").unwrap_or((line, ""));
        let mut values = (None, None, None, None);
        for field in fields.split_whitespace() {
            match field.split_once('=') {
                Some(("signal", value)) => values.0 = value.parse::<i32>().ok(),
                Some(("worker_pid", value)) => values.1 = value.parse::<u32>().ok(),
                Some(("sender_pid", value)) => values.2 = value.parse().ok(),
                Some(("sender_uid", value)) => values.3 = value.parse().ok(),
                _ => {}
            }
        }
        let (Some(sent), Some(receiver), Some(pid), Some(uid)) = values else {
            return None;
        };
        (sent == signal && receiver == worker_pid).then(|| Self {
            pid,
            uid,
            command: Some(command.trim())
                .filter(|command| !command.is_empty())
                .map(str::to_string),
        })
    }
}

impl WorkerExit {
    /// The exit `status` of `worker_pid`, with what its scope reports now
    /// and the signal sender its log names.
    pub(crate) fn observe(
        worker_pid: u32,
        status: &ExitStatus,
        scope: Option<&WorkerScopeCgroup>,
        worker_log_tail: Option<&str>,
    ) -> Self {
        #[cfg(unix)]
        let (signal, core_dumped) = {
            use std::os::unix::process::ExitStatusExt;
            (status.signal(), status.core_dumped())
        };
        #[cfg(not(unix))]
        let (signal, core_dumped) = (None, false);
        Self {
            worker_pid,
            exit_status: status.to_string(),
            exit_code: status.code(),
            signal,
            signal_name: signal.and_then(signal_name).map(str::to_string),
            core_dumped,
            scope: scope.map(|scope| ScopeEvidence::read(scope, false)),
            signal_sender: signal.and_then(|signal| {
                SignalSender::from_worker_log(worker_log_tail?, worker_pid, signal)
            }),
            observed_at: Utc::now(),
        }
    }
}

/// Append `exit` to the diagnostic reconciliation wrote when it interrupted
/// `run_id` before this supervisor reaped `exit.worker_pid`. A run another
/// path settled, or a diagnostic that already names an exit, is left alone.
/// Reconciliation writes its step just after the interrupted state, so a
/// missing step is waited for briefly.
pub(crate) fn note_exit_after_reconciliation(
    runs: &dyn JobRunStoreBackend,
    run_id: &str,
    exit: &WorkerExit,
) -> Result<(), OrbitError> {
    let deadline = Instant::now() + RECONCILED_STEP_WAIT;
    loop {
        let Some(run) = runs.get_job_run(run_id)? else {
            return Ok(());
        };
        if run.state != JobRunState::Interrupted || run.pid != Some(exit.worker_pid) {
            return Ok(());
        }
        let error_step = run
            .steps
            .iter()
            .find(|step| step.error_code.is_some() || step.error_message.is_some());
        let Some(step) = error_step else {
            if Instant::now() >= deadline {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(50));
            continue;
        };
        let Some(message) = step.error_message.as_deref() else {
            return Ok(());
        };
        if step.error_code.as_deref() != Some(WORKER_TERMINATED_ERROR_CODE)
            || !message.contains(NO_EXIT_OBSERVED)
            || message.contains(OBSERVED_AFTERWARDS)
        {
            return Ok(());
        }
        let Some(started_at) = step.started_at else {
            return Ok(());
        };
        let finished_at = step.finished_at.unwrap_or(started_at);
        let amended = format!("{message}; afterwards {}", exit.describe());
        runs.complete_job_run_step(
            run_id,
            &JobRunStepParams {
                lock_wait_ms: step.lock_wait_ms,
                step_index: step.step_index as usize,
                target_type: step.target_type,
                target_id: step.target_id.clone(),
                started_at,
                finished_at,
                duration_ms: step.duration_ms,
                exit_code: step.exit_code,
                agent_response_json: step.agent_response_json.clone(),
                state: step.state,
                error_code: step.error_code.clone(),
                error_message: Some(amended),
            },
        )?;
        return Ok(());
    }
}

/// Prefix of the line the worker's handler writes for a signal another
/// process sent it.
const SIGNAL_SENDER_MARKER: &str = "orbit pipeline worker terminating signal:";

/// Catch SIGTERM, SIGINT and SIGHUP for the rest of this worker process's
/// life, where each still has its default disposition, to name the sender of
/// the one that ends it. The handler writes one line to stderr, the worker
/// log, then restores the default disposition and re-raises, so the worker
/// still dies from that signal with the same exit status. A signal this
/// process raised itself (a supervised step's signal guard forwarding one
/// it intercepted) names no sender. Other platforms capture none.
pub(crate) fn capture_terminating_signal_senders() {
    #[cfg(target_os = "linux")]
    sender_capture::install();
}

#[cfg(target_os = "linux")]
mod sender_capture {
    use std::sync::Once;

    use super::SIGNAL_SENDER_MARKER;

    const SIGNALS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP];
    static INSTALL: Once = Once::new();

    pub(super) fn install() {
        INSTALL.call_once(|| {
            for signal in SIGNALS {
                // SAFETY: `sigaction` only reads and replaces this process's
                // disposition of `signal`. An ignored signal, or one another
                // handler owns, is left as it is.
                unsafe {
                    let mut current: libc::sigaction = std::mem::zeroed();
                    if libc::sigaction(signal, std::ptr::null(), &mut current) != 0
                        || current.sa_sigaction != libc::SIG_DFL
                    {
                        continue;
                    }
                    let mut action: libc::sigaction = std::mem::zeroed();
                    action.sa_sigaction = record_sender_and_terminate as *const () as usize;
                    action.sa_flags = libc::SA_SIGINFO;
                    libc::sigfillset(&mut action.sa_mask);
                    if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                        tracing::warn!(
                            signal,
                            error = %std::io::Error::last_os_error(),
                            "pipeline worker cannot capture its terminating signal's sender",
                        );
                    }
                }
            }
        });
    }

    /// Async-signal-safe: only `getpid`, `open`, `read`, `close`, `write`,
    /// `sigaction` and `raise`, with no allocation or lock.
    extern "C" fn record_sender_and_terminate(
        signal: libc::c_int,
        info: *mut libc::siginfo_t,
        _context: *mut libc::c_void,
    ) {
        // SAFETY: the kernel passes a valid `siginfo_t` to an `SA_SIGINFO`
        // handler; `si_pid`/`si_uid` are meaningful for the user-sent codes
        // checked first.
        unsafe {
            let own_pid = libc::getpid();
            if let Some(info) = info.as_ref()
                && matches!(
                    info.si_code,
                    libc::SI_USER | libc::SI_QUEUE | libc::SI_TKILL
                )
                && info.si_pid() > 0
                && info.si_pid() != own_pid
            {
                write_sender(signal, own_pid, info.si_pid(), info.si_uid());
            }
            // A freshly zeroed action, so `SIG_DFL` keeps no `SA_SIGINFO`.
            let mut default: libc::sigaction = std::mem::zeroed();
            default.sa_sigaction = libc::SIG_DFL;
            libc::sigemptyset(&mut default.sa_mask);
            libc::sigaction(signal, &default, std::ptr::null_mut());
            // Blocked until this handler returns, then delivered with the
            // default action, which terminates the process.
            libc::raise(signal);
        }
    }

    /// One fixed-format line, built on the stack.
    fn write_sender(
        signal: libc::c_int,
        worker_pid: libc::pid_t,
        sender_pid: libc::pid_t,
        sender_uid: libc::uid_t,
    ) {
        let mut line = Line::default();
        line.push(b"\n");
        line.push(SIGNAL_SENDER_MARKER.as_bytes());
        line.push(b" signal=");
        line.push_number(signal as u64);
        line.push(b" worker_pid=");
        line.push_number(worker_pid as u64);
        line.push(b" sender_pid=");
        line.push_number(sender_pid as u64);
        line.push(b" sender_uid=");
        line.push_number(u64::from(sender_uid));
        line.push(b" sender_comm=");
        let mut path = Line::default();
        path.push(b"/proc/");
        path.push_number(sender_pid as u64);
        path.push(b"/comm\0");
        let mut comm = [0u8; 64];
        // SAFETY: `path` is NUL-terminated and `comm` outlives the read.
        let read = unsafe {
            let fd = libc::open(
                path.bytes().as_ptr().cast(),
                libc::O_RDONLY | libc::O_CLOEXEC,
            );
            if fd < 0 {
                0
            } else {
                let read = libc::read(fd, comm.as_mut_ptr().cast(), comm.len());
                libc::close(fd);
                read.max(0) as usize
            }
        };
        for byte in comm[..read].iter_mut() {
            if !(0x20..0x7f).contains(byte) {
                *byte = b' ';
            }
        }
        line.push(&comm[..read]);
        line.push(b"\n");
        // SAFETY: fd 2 is the worker log; a failed write is ignored.
        unsafe {
            libc::write(2, line.bytes().as_ptr().cast(), line.bytes().len());
        }
    }

    struct Line {
        bytes: [u8; 192],
        len: usize,
    }

    impl Default for Line {
        fn default() -> Self {
            Self {
                bytes: [0; 192],
                len: 0,
            }
        }
    }

    impl Line {
        fn push(&mut self, bytes: &[u8]) {
            let room = self.bytes.len() - self.len;
            let take = bytes.len().min(room);
            self.bytes[self.len..self.len + take].copy_from_slice(&bytes[..take]);
            self.len += take;
        }

        fn push_number(&mut self, mut value: u64) {
            let mut digits = [0u8; 20];
            let mut start = digits.len();
            loop {
                start -= 1;
                digits[start] = b'0' + (value % 10) as u8;
                value /= 10;
                if value == 0 {
                    break;
                }
            }
            self.push(&digits[start..]);
        }

        fn bytes(&self) -> &[u8] {
            &self.bytes[..self.len]
        }
    }
}

/// The conventional name of a terminating signal.
#[cfg(unix)]
fn signal_name(signal: i32) -> Option<&'static str> {
    Some(match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGTRAP => "SIGTRAP",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        libc::SIGXCPU => "SIGXCPU",
        libc::SIGXFSZ => "SIGXFSZ",
        libc::SIGSYS => "SIGSYS",
        _ => return None,
    })
}

#[cfg(not(unix))]
fn signal_name(_signal: i32) -> Option<&'static str> {
    None
}
