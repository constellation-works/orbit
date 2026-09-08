use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tempfile::TempDir;
use tracing::field::{Field, Visit};
use tracing::span;
use tracing::{Event, Metadata, Subscriber};

use super::super::{
    FileLockGuard, LockOptions, acquire_exclusive, acquire_exclusive_with, read_lock_holder,
};

/// Exact libtest name of the ignored helper below, re-exec'd as a child process
/// by [`sigkilled_holder_releases_lock`]. Keep in sync with the module path.
#[cfg(unix)]
const CRASH_CHILD_TEST: &str = "fs::lock::tests::file_lock::crash_holder_child";

fn short_options(timeout_ms: u64) -> LockOptions {
    LockOptions {
        timeout: Duration::from_millis(timeout_ms),
        // Push the warn threshold out of the way unless a test wants it.
        warn_after: Duration::from_secs(3600),
    }
}

#[test]
fn acquire_release_then_reacquire() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("state").join("x.lock");

    {
        let _guard: FileLockGuard = acquire_exclusive(&path, "first").expect("first acquire");
        assert_eq!(
            read_lock_holder(&path).expect("holder while held").label,
            "first"
        );
    } // guard dropped here -> flock released

    assert!(
        read_lock_holder(&path).is_none(),
        "clean release must clear diagnostic holder metadata"
    );

    // Parent dirs were created by the first acquisition; a second one succeeds.
    let _guard = acquire_exclusive(&path, "second").expect("second acquire after release");
}

#[test]
fn times_out_naming_lock_path_and_holder() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("a.lock");

    let _held = acquire_exclusive(&path, "holder").expect("hold lock");

    // A second independent descriptor conflicts even within one process
    // (flock(2) treats separate opens independently), so this must time out.
    let error = acquire_exclusive_with(&path, "waiter", short_options(150))
        .expect_err("acquisition should time out while the lock is held");
    let timeout = error
        .file_lock_timeout()
        .expect("timeout must be a structured OrbitError variant");
    assert_eq!(timeout.lock_path, path);
    assert_eq!(timeout.label, "waiter");
    assert_eq!(
        timeout.holder.as_ref().map(|holder| holder.label.as_str()),
        Some("holder")
    );
    let message = error.to_string();

    assert!(
        message.contains("a.lock"),
        "timeout error must name the lock path: {message}"
    );
    assert!(
        message.contains(&format!("pid {}", std::process::id())),
        "timeout error must name the holder pid: {message}"
    );
    assert!(
        message.contains("op: holder"),
        "timeout error must carry the holder's operation label: {message}"
    );
}

#[test]
fn blocked_waiter_emits_warning_with_holder_metadata() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("b.lock");

    let _held = acquire_exclusive(&path, "holder").expect("hold lock");

    let events = Arc::new(Mutex::new(Vec::new()));
    let subscriber = CaptureSubscriber {
        events: Arc::clone(&events),
    };
    let dispatch = tracing::Dispatch::new(subscriber);

    let options = LockOptions {
        timeout: Duration::from_millis(400),
        warn_after: Duration::from_millis(20),
    };
    let result = tracing::dispatcher::with_default(&dispatch, || {
        acquire_exclusive_with(&path, "waiter", options)
    });
    assert!(result.is_err(), "held lock should still time out");

    let events = events.lock().expect("events lock");
    let warned = events.iter().any(|fields| {
        fields.get("label").map(String::as_str) == Some("waiter")
            && fields
                .get("holder")
                .is_some_and(|holder| holder.contains(&format!("pid {}", std::process::id())))
            && fields.contains_key("waited_ms")
    });
    assert!(
        warned,
        "expected a warn event naming the waiter and holder metadata; captured={events:?}"
    );
}

#[test]
fn read_lock_holder_reports_current_holder() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("h.lock");

    let _held = acquire_exclusive(&path, "doctor scan").expect("hold lock");

    let holder = read_lock_holder(&path).expect("holder metadata present while held");
    assert_eq!(holder.pid, std::process::id());
    assert_eq!(holder.label, "doctor scan");
    assert!(
        !holder.acquired_at.is_empty(),
        "acquired_at should carry the RFC 3339 acquisition timestamp"
    );
}

#[test]
fn read_lock_holder_is_lenient_on_missing_or_garbage_files() {
    let dir = TempDir::new().expect("tempdir");

    // Missing file → None, not an error.
    assert!(read_lock_holder(&dir.path().join("absent.lock")).is_none());

    // Torn/legacy content → None, not an error.
    let garbage = dir.path().join("garbage.lock");
    std::fs::write(&garbage, b"not json").expect("write garbage");
    assert!(read_lock_holder(&garbage).is_none());
}

/// Crash semantics: the OS releases advisory (`flock`) locks when a holder
/// process dies, so a hung/crashed holder never wedges the workspace forever.
/// A child process takes the lock, we SIGKILL it, and a subsequent acquisition
/// by the parent must succeed. Documents the assumption the timeout targets the
/// *hung* (not crashed) holder.
#[cfg(unix)]
#[test]
fn sigkilled_holder_releases_lock() {
    use std::os::unix::fs::MetadataExt;

    let dir = TempDir::new().expect("tempdir");
    let lock_path = dir.path().join("crash.lock");
    let ready_path = dir.path().join("ready");

    let exe = std::env::current_exe().expect("current test exe");
    let mut child = std::process::Command::new(exe)
        .args(["--exact", CRASH_CHILD_TEST, "--ignored"])
        .env("ORBIT_FILE_LOCK_CRASH_LOCK", &lock_path)
        .env("ORBIT_FILE_LOCK_CRASH_READY", &ready_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn lock-holder child");

    // Wait for the child to signal it holds the lock.
    let start = std::time::Instant::now();
    while !ready_path.exists() {
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child never acquired the lock");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let child_pid = child.id();
    let inode = std::fs::metadata(&lock_path).expect("lock metadata").ino();

    // While the child holds it, the parent cannot acquire within a short budget.
    let held = acquire_exclusive_with(&lock_path, "parent-probe", short_options(200));
    assert!(held.is_err(), "lock should be held by the live child");

    // Crash the holder: Child::kill() sends SIGKILL on Unix.
    child.kill().expect("SIGKILL child");
    child.wait().expect("reap child");
    let stale = read_lock_holder(&lock_path).expect("crash leaves diagnostic metadata");
    assert_eq!(stale.pid, child_pid);
    assert_eq!(stale.label, "crash-holder");

    // The advisory lock is released on process death: acquisition now succeeds.
    let guard = acquire_exclusive_with(
        &lock_path,
        "parent-after-crash",
        LockOptions {
            timeout: Duration::from_secs(10),
            warn_after: Duration::from_secs(3600),
        },
    );
    let guard = guard.expect("lock released after holder was SIGKILLed");
    assert_eq!(
        std::fs::metadata(&lock_path).expect("lock retained").ino(),
        inode,
        "recovery must acquire the existing lock file"
    );
    drop(guard);
    assert!(
        read_lock_holder(&lock_path).is_none(),
        "clean reacquisition clears the stale crash metadata"
    );
}

/// Re-exec'd as a child process by [`sigkilled_holder_releases_lock`]. Ignored
/// so it never runs on its own; when invoked with the crash env vars it takes
/// the lock, signals readiness via a sentinel file, and blocks until killed.
#[cfg(unix)]
#[test]
#[ignore = "helper process for sigkilled_holder_releases_lock; re-exec'd, not run directly"]
fn crash_holder_child() {
    let (Ok(lock_path), Ok(ready_path)) = (
        std::env::var("ORBIT_FILE_LOCK_CRASH_LOCK"),
        std::env::var("ORBIT_FILE_LOCK_CRASH_READY"),
    ) else {
        return;
    };

    let _guard =
        acquire_exclusive(std::path::Path::new(&lock_path), "crash-holder").expect("child acquire");
    std::fs::write(&ready_path, b"ready").expect("write ready sentinel");
    // Hold the lock until the parent SIGKILLs us.
    std::thread::sleep(Duration::from_secs(60));
}

/// Minimal tracing subscriber that records each event's fields into a shared
/// vector, so a test can assert on emitted warnings. Mirrors the capture
/// pattern in `orbit-engine`'s cli_runner tests.
struct CaptureSubscriber {
    events: Arc<Mutex<Vec<BTreeMap<String, String>>>>,
}

impl Subscriber for CaptureSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}

    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut visitor = FieldCapture::default();
        event.record(&mut visitor);
        self.events
            .lock()
            .expect("events lock")
            .push(visitor.fields);
    }

    fn enter(&self, _span: &span::Id) {}

    fn exit(&self, _span: &span::Id) {}
}

#[derive(Default)]
struct FieldCapture {
    fields: BTreeMap<String, String>,
}

impl Visit for FieldCapture {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields
            .insert(field.name().to_string(), format!("{value:?}"));
    }
}
