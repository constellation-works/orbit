use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use orbit_common::OrbitError;

/// Slots for live child process groups. The signal handler walks this table
/// and therefore cannot take a mutex; empty is `0` (never a valid pgid here,
/// because children are spawned as their own group leaders).
const MAX_LIVE_PROCESS_GROUPS: usize = 256;

static HANDLER_INSTALL: OnceLock<Mutex<HandlerInstall>> = OnceLock::new();
static LAST_SIGNAL: AtomicI32 = AtomicI32::new(0);
static SIGNAL_GEN: AtomicU64 = AtomicU64::new(0);
static LIVE_PGIDS: [AtomicU32; MAX_LIVE_PROCESS_GROUPS] =
    [const { AtomicU32::new(0) }; MAX_LIVE_PROCESS_GROUPS];

struct HandlerInstall {
    refcount: usize,
    previous: Option<PreviousHandlers>,
}

struct PreviousHandlers {
    sigint: libc::sigaction,
    sigterm: libc::sigaction,
}

/// Process-wide SIGINT/SIGTERM intercept for the duration of one supervised
/// wait. Install is refcounted: the first live guard swaps in the handlers,
/// and the last drop restores the previous dispositions. The install mutex
/// is held only for that refcount/sigaction critical section — never across
/// the child's lifetime — so concurrent supervisors overlap.
pub(super) struct SignalHandlerGuard {
    start_gen: u64,
    slot: Option<usize>,
}

impl SignalHandlerGuard {
    pub(super) fn install(pgid: u32) -> Result<Self, OrbitError> {
        let start_gen = acquire_handlers()?;
        Ok(Self {
            start_gen,
            slot: register_pgid(pgid),
        })
    }

    pub(super) fn take_signal(&self) -> Option<i32> {
        if SIGNAL_GEN.load(Ordering::SeqCst) == self.start_gen {
            return None;
        }
        let signal = LAST_SIGNAL.load(Ordering::SeqCst);
        (signal != 0).then_some(signal)
    }
}

impl Drop for SignalHandlerGuard {
    fn drop(&mut self) {
        unregister_pgid(self.slot);
        release_handlers();
    }
}

pub(super) fn signal_message(signal: i32) -> String {
    format!("process interrupted by signal {}", signal_name(signal))
}

fn signal_name(signal: i32) -> &'static str {
    match signal {
        libc::SIGINT => "SIGINT",
        libc::SIGTERM => "SIGTERM",
        libc::SIGKILL => "SIGKILL",
        _ => "UNKNOWN",
    }
}

fn acquire_handlers() -> Result<u64, OrbitError> {
    let mut state = handler_install()
        .lock()
        .map_err(|_| OrbitError::Execution("signal handler lock poisoned".to_string()))?;

    // Snapshot before this waiter is live so a signal that arrives during
    // first-install still looks newer than `start_gen` on the first poll.
    let start_gen = SIGNAL_GEN.load(Ordering::SeqCst);
    if state.refcount == 0 {
        let sigint = install_signal_handler(libc::SIGINT)?;
        let sigterm = match install_signal_handler(libc::SIGTERM) {
            Ok(previous) => previous,
            Err(err) => {
                restore_signal_handler(libc::SIGINT, &sigint);
                return Err(err);
            }
        };
        state.previous = Some(PreviousHandlers { sigint, sigterm });
    }

    state.refcount = state
        .refcount
        .checked_add(1)
        .ok_or_else(|| OrbitError::Execution("signal handler refcount overflow".to_string()))?;
    Ok(start_gen)
}

fn release_handlers() {
    let Ok(mut state) = handler_install().lock() else {
        return;
    };
    if state.refcount == 0 {
        return;
    }
    state.refcount -= 1;
    if state.refcount > 0 {
        return;
    }
    if let Some(previous) = state.previous.take() {
        restore_signal_handler(libc::SIGINT, &previous.sigint);
        restore_signal_handler(libc::SIGTERM, &previous.sigterm);
    }
}

fn handler_install() -> &'static Mutex<HandlerInstall> {
    HANDLER_INSTALL.get_or_init(|| {
        Mutex::new(HandlerInstall {
            refcount: 0,
            previous: None,
        })
    })
}

fn register_pgid(pgid: u32) -> Option<usize> {
    if pgid == 0 {
        return None;
    }
    for (index, slot) in LIVE_PGIDS.iter().enumerate() {
        if slot
            .compare_exchange(0, pgid, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return Some(index);
        }
    }
    None
}

fn unregister_pgid(slot: Option<usize>) {
    if let Some(index) = slot {
        LIVE_PGIDS[index].store(0, Ordering::SeqCst);
    }
}

unsafe extern "C" fn termination_signal_handler(signal: libc::c_int) {
    LAST_SIGNAL.store(signal, Ordering::SeqCst);
    SIGNAL_GEN.fetch_add(1, Ordering::SeqCst);
    for slot in &LIVE_PGIDS {
        let pgid = slot.load(Ordering::Relaxed);
        if pgid != 0 {
            // Safety: `killpg` is async-signal-safe. `pgid` is a live child's
            // process-group id stored by a supervisor, or a stale id of a
            // group that already exited (`ESRCH` is ignored).
            unsafe {
                libc::killpg(pgid as libc::pid_t, signal);
            }
        }
    }
}

fn install_signal_handler(signal: libc::c_int) -> Result<libc::sigaction, OrbitError> {
    // Safety: sigaction installs a process signal handler. The handler only
    // stores atomics and calls `killpg`, both async-signal-safe.
    unsafe {
        let mut new_action: libc::sigaction = std::mem::zeroed();
        new_action.sa_sigaction = termination_signal_handler as *const () as usize;
        new_action.sa_flags = 0;
        libc::sigemptyset(&mut new_action.sa_mask);
        libc::sigaddset(&mut new_action.sa_mask, libc::SIGINT);
        libc::sigaddset(&mut new_action.sa_mask, libc::SIGTERM);

        let mut old_action: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(signal, &new_action, &mut old_action) != 0 {
            return Err(OrbitError::Execution(format!(
                "failed to install signal handler for {}: {}",
                signal_name(signal),
                std::io::Error::last_os_error()
            )));
        }

        Ok(old_action)
    }
}

fn restore_signal_handler(signal: libc::c_int, previous: &libc::sigaction) {
    // Safety: restores the exact handler previously returned by sigaction.
    unsafe {
        libc::sigaction(signal, previous, std::ptr::null_mut());
    }
}
