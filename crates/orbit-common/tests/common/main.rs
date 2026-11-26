//! Orbit-common integration coverage in one area binary.
//!
//! Filesystem, process, generation and test-environment cases share this
//! target. None of them installs a signal handler or mutates the process
//! environment outside `orbit_common::test_env`, so a separate binary is
//! not warranted. One load-time scrub covers the process.

#![allow(clippy::expect_used, clippy::unwrap_used, missing_docs)]

#[path = "mechanisms/env.rs"]
mod env;
#[path = "mechanisms/fs.rs"]
mod fs;
#[path = "mechanisms/governance.rs"]
mod governance;
#[path = "mechanisms/migration.rs"]
mod migration;
#[path = "mechanisms/observability.rs"]
mod observability;
#[cfg(unix)]
#[path = "mechanisms/process.rs"]
mod process;
#[path = "mechanisms/security.rs"]
mod security;
#[cfg(feature = "sqlite")]
#[path = "mechanisms/sqlite.rs"]
mod sqlite;
#[path = "mechanisms/storage.rs"]
mod storage;

// This binary owns its allocator so allocation volume can detect repeated
// carry copies and quadratic redaction scans deterministically, without timing
// limits or synthetic load.
mod counting_allocator {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        pub(super) static ALLOCATED: Cell<Option<usize>> = const { Cell::new(None) };
    }

    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    struct CountingAllocator;

    // SAFETY: Every allocation operation delegates unchanged to System. The
    // thread-local counter is allocation-free and does not inspect pointers.
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            count_allocation(layout.size());
            // SAFETY: The caller supplies the layout required by System.
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            count_allocation(layout.size());
            // SAFETY: The caller supplies the layout required by System.
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: The pointer and layout are forwarded to their allocator.
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            count_allocation(new_size);
            // SAFETY: The original pointer, layout and new size are unchanged.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    fn count_allocation(size: usize) {
        let _ = ALLOCATED.try_with(|count| {
            if let Some(total) = count.get() {
                count.set(Some(total.saturating_add(size)));
            }
        });
    }
}

/// ORB-15240: nextest interrupting a test parent must not leave the fixture
/// child group it spawned running. The parent is SIGKILLed mid-run, which
/// gives it no chance to run its own cleanup.
#[cfg(unix)]
mod orphaned_fixture_children {
    use super::*;
    use orbit_common::test_env::{PsRun, ps_stat, report_ps_denied, run_child_test_within};
    use std::io::Write;
    use std::path::Path;
    use std::time::Instant;

    const DIR_ENV: &str = "ORBIT_ORPHAN_FIXTURE_DIR";
    const CHATTY_SECS_ENV: &str = "ORBIT_CHATTY_CHILD_SECS";
    const CHATTY_MILLIS_ENV: &str = "ORBIT_CHATTY_CHILD_MILLIS";
    const CHATTY_FIRST_PRINT_DELAY_ENV: &str = "ORBIT_CHATTY_CHILD_FIRST_PRINT_DELAY_MS";
    /// Long enough that a survivor is unmistakable, short enough that a failed
    /// run does not leave processes for long.
    const SURVIVOR_SECS: &str = "120";
    const BOUND: Duration = Duration::from_secs(20);

    fn reexec(entry: &str, dir: &Path) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                &format!("orphaned_fixture_children::{entry}"),
                "--ignored",
                "--nocapture",
            ])
            .env(DIR_ENV, dir);
        command
    }

    fn wait_for_pid(path: &Path) -> u32 {
        let started = Instant::now();
        loop {
            if let Some(pid) = std::fs::read_to_string(path)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                return pid;
            }
            assert!(
                started.elapsed() < BOUND,
                "{} never written",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Running and not a zombie: a killed process nobody has reaped yet is gone.
    fn is_running(pid: u32) -> Result<bool, String> {
        let output = match ps_stat(pid) {
            PsRun::Ran(output) => output,
            PsRun::Denied(reason) => return Err(reason),
        };
        assert!(
            output.status.success()
                || (output.status.code() == Some(1)
                    && output.stdout.is_empty()
                    && output.stderr.is_empty()),
            "ps failed: {output:?}"
        );
        let state = String::from_utf8_lossy(&output.stdout);
        let state = state.trim();
        Ok(!state.is_empty() && !state.starts_with('Z'))
    }

    // Probe before spawning long-lived fixtures, so a denied ps leaves no
    // children behind. Later denials still propagate through fixture cleanup.
    fn ps_available(test: &str) -> bool {
        match is_running(std::process::id()) {
            Ok(running) => {
                assert!(running, "ps must see this live test process");
                true
            }
            Err(reason) => {
                report_ps_denied(test, &reason);
                false
            }
        }
    }

    fn assert_gone_within_bound(what: &str, pid: u32) -> Result<(), String> {
        let started = Instant::now();
        while is_running(pid)? {
            assert!(
                started.elapsed() < BOUND,
                "{what} (pid {pid}) still running {BOUND:?} after its test parent died"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    fn kill_leftovers(pids: &[u32]) {
        for pid in pids {
            // Best effort so a failed assertion does not leave the sleepers.
            let _ = Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
    }

    #[test]
    fn a_killed_test_parent_takes_its_fixture_child_group_with_it() {
        let test =
            "orphaned_fixture_children::a_killed_test_parent_takes_its_fixture_child_group_with_it";
        if !ps_available(test) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut parent = reexec("orphan_parent_entry", dir.path());
        let mut parent = parent
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let child = wait_for_pid(&dir.path().join("child.pid"));
        let grandchild = wait_for_pid(&dir.path().join("grandchild.pid"));
        let running = is_running(child)
            .and_then(|child| is_running(grandchild).map(|grandchild| child && grandchild));

        parent.kill().unwrap();
        parent.wait().unwrap();

        let outcome = std::panic::catch_unwind(|| {
            assert!(running?, "fixture started");
            assert_gone_within_bound("fixture child", child)?;
            assert_gone_within_bound("fixture grandchild", grandchild)
        });
        if !matches!(outcome, Ok(Ok(()))) {
            kill_leftovers(&[child, grandchild]);
        }
        if let Err(reason) = outcome.unwrap() {
            report_ps_denied(test, &reason);
        }
    }

    #[test]
    fn an_overrunning_child_is_reported_with_its_output_and_its_group_is_killed() {
        let test = "orphaned_fixture_children::an_overrunning_child_is_reported_with_its_output_and_its_group_is_killed";
        if !ps_available(test) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut command = reexec("orphan_child_entry", dir.path());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_child_test_within(
                &mut command,
                "orphan_child_entry",
                dir.path(),
                Duration::from_secs(2),
                Duration::from_secs(60),
            )
        }));
        let message = panic_message(result.expect_err("an overrunning child fails the caller"));
        for expected in [
            "orphan_child_entry",
            "printed nothing for 2s",
            "host load",
            "fixture child running",
        ] {
            assert!(message.contains(expected), "{expected:?} in {message}");
        }
        let child = wait_for_pid(&dir.path().join("child.pid"));
        let grandchild = wait_for_pid(&dir.path().join("grandchild.pid"));
        let outcome = std::panic::catch_unwind(|| {
            assert_gone_within_bound("overrun child", child)?;
            assert_gone_within_bound("overrun grandchild", grandchild)
        });
        if !matches!(outcome, Ok(Ok(()))) {
            kill_leftovers(&[child, grandchild]);
        }
        if let Err(reason) = outcome.unwrap() {
            report_ps_denied(test, &reason);
        }
    }

    /// ORB-15293: a child that keeps reporting progress is slow, not hung, so
    /// the quiet deadline must not kill it however long it runs in total.
    /// ORB-16411: that deadline opens at the first byte, so the quiet window is
    /// the gap between lines, not an allowance for exec and libtest startup.
    #[test]
    fn a_child_that_keeps_printing_outlives_its_quiet_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let mut command = reexec("chatty_child_entry", dir.path());
        // ORB-25251: macOS CI killed this progressing child at the old
        // 300 ms window. Allow seconds between progress lines, while keeping
        // the child alive for twice the quiet window to require resets.
        let quiet = Duration::from_secs(2);
        let run_for = quiet * 2;
        command.env(CHATTY_MILLIS_ENV, run_for.as_millis().to_string());
        let started = Instant::now();
        let output = run_child_test_within(
            &mut command,
            "orphaned_fixture_children::chatty_child_entry",
            dir.path(),
            quiet,
            Duration::from_secs(120),
        );
        assert!(
            started.elapsed() > quiet,
            "child ran past the quiet deadline"
        );
        assert_child_test_passed(
            "chatty_child_entry",
            output.status,
            &output.stdout,
            &output.stderr,
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .filter(|line| *line == "chatty progress")
                .count()
                >= 2,
            "child must report repeated progress while outliving its quiet window"
        );
    }

    /// ORB-16411: the quiet window used to include exec and libtest startup, so
    /// a 300 ms bound killed a healthy child on a loaded host. ORB-17001: the
    /// libtest header is itself the first byte, so the child can still be
    /// descheduled before its first progress line. Keep this fixture's quiet
    /// window wider than that scheduling gap and exercise the gap directly.
    #[test]
    fn a_child_whose_first_line_arrives_after_the_quiet_window_is_not_hung() {
        let dir = tempfile::tempdir().unwrap();
        let exe = std::env::current_exe().unwrap();
        let exe = exe.to_str().unwrap();
        let entry = "orphaned_fixture_children::chatty_child_entry";
        // The shell stays silent longer than `quiet`, then writes the first
        // controlled byte before exec starts libtest. The test body then waits
        // 500 ms before its first progress line, without letting the libtest
        // header start the tested quiet window or adding synthetic host load.
        let script = format!(
            "sleep 1.1; printf '%s\\n' 'controlled pre-libtest progress' >&2; exec {} --exact {} --ignored --nocapture",
            quote_posix_arg(exe),
            quote_posix_arg(entry),
        );
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &script]);
        command.env(CHATTY_MILLIS_ENV, "600");
        command.env(CHATTY_FIRST_PRINT_DELAY_ENV, "500");
        let quiet = Duration::from_secs(1);
        let started = Instant::now();
        let output = run_child_test_within(
            &mut command,
            entry,
            dir.path(),
            quiet,
            Duration::from_secs(120),
        );
        assert!(
            started.elapsed() >= Duration::from_millis(2_200),
            "the pre-output delay and the print budget must both elapse"
        );
        assert_child_test_passed(
            "chatty_child_entry",
            output.status,
            &output.stdout,
            &output.stderr,
        );
        assert!(
            output
                .stderr
                .starts_with(b"controlled pre-libtest progress\n"),
            "the shell's controlled progress must precede libtest startup"
        );
    }

    /// Progress buys time only up to the ceiling, which stays below nextest's
    /// termination so the parent still reports what the child printed.
    #[test]
    fn a_child_printing_past_its_ceiling_is_killed_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let mut command = reexec("chatty_child_entry", dir.path());
        command.env(CHATTY_SECS_ENV, SURVIVOR_SECS);
        let started = Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_child_test_within(
                &mut command,
                "chatty_child_entry",
                dir.path(),
                Duration::from_secs(60),
                Duration::from_secs(2),
            )
        }));
        let message = panic_message(result.expect_err("a child past its ceiling fails the caller"));
        for expected in ["chatty_child_entry", "2s ceiling", "chatty progress"] {
            assert!(message.contains(expected), "{expected:?} in {message}");
        }
        assert!(
            started.elapsed() < BOUND,
            "the ceiling, not the child, ended the run"
        );
    }

    /// Prints progress often enough to reset a quiet deadline, for
    /// `CHATTY_MILLIS_ENV` milliseconds or else `CHATTY_SECS_ENV` seconds.
    #[test]
    #[ignore = "re-executed by the child progress regressions"]
    fn chatty_child_entry() {
        let budget = match std::env::var(CHATTY_MILLIS_ENV) {
            Ok(milliseconds) => Duration::from_millis(milliseconds.parse().expect("chatty millis")),
            Err(_) => Duration::from_secs(std::env::var(CHATTY_SECS_ENV).unwrap().parse().unwrap()),
        };
        if let Ok(milliseconds) = std::env::var(CHATTY_FIRST_PRINT_DELAY_ENV) {
            std::thread::sleep(Duration::from_millis(
                milliseconds.parse().expect("first print delay millis"),
            ));
        }
        // A sub-second run prints every 15 ms so a few hundred milliseconds
        // of quiet still sees progress. Longer runs keep the 200 ms gap.
        let pace = if budget < Duration::from_secs(1) {
            Duration::from_millis(15)
        } else {
            Duration::from_millis(200)
        };
        let started = Instant::now();
        while started.elapsed() < budget {
            let _ = writeln!(std::io::stderr(), "chatty progress");
            std::thread::sleep(pace);
        }
    }

    /// Plays the nextest-owned test process: runs a fixture child and waits.
    #[test]
    #[ignore = "re-executed by the killed-parent regression"]
    fn orphan_parent_entry() {
        let dir = std::path::PathBuf::from(std::env::var_os(DIR_ENV).unwrap());
        let mut command = reexec("orphan_child_entry", &dir);
        run_child_test(&mut command, "orphan_child_entry", &dir);
    }

    /// The fixture child: records its pid, spawns a descendant, then lingers.
    #[test]
    #[ignore = "re-executed by the killed-parent regression"]
    fn orphan_child_entry() {
        let dir = std::path::PathBuf::from(std::env::var_os(DIR_ENV).unwrap());
        let mut grandchild = Command::new("sleep").arg(SURVIVOR_SECS).spawn().unwrap();
        std::fs::write(dir.join("grandchild.pid"), grandchild.id().to_string()).unwrap();
        std::fs::write(dir.join("child.pid"), std::process::id().to_string()).unwrap();
        let _ = writeln!(std::io::stdout(), "fixture child running");
        grandchild.wait().unwrap();
    }
}

/// Test runs skip durability syncs: on macOS each one is an `F_FULLFSYNC` that
/// flushes the whole drive, and Mac gates ran 5-20x slower than Linux for it.
/// A sync on a pipe fails on every Unix, so whether the helper reached the
/// kernel is observable without a power cut.
#[cfg(unix)]
mod durability_syncs {
    use std::ffi::OsStr;
    use std::fs::File;
    use std::os::fd::OwnedFd;
    use std::process::Command;

    use orbit_common::fs::io::{
        RELAXED_DURABILITY_ENV, relaxed_durability_requested, sqlite_synchronous, sync_file,
        sync_file_data,
    };
    use orbit_common::test_env::{assert_child_test_passed, require_durable_syncs, run_child_test};

    const CHILD: &str = "durability_syncs::sync_probe_child";
    const EXPECT: &str = "ORBIT_DURABILITY_PROBE_EXPECT";

    #[test]
    fn a_release_build_ignores_the_relaxed_durability_request() {
        let relaxed = Some(OsStr::new("1"));
        assert!(
            !relaxed_durability_requested(relaxed, false),
            "a release or installed binary must sync whatever its environment says"
        );
        assert!(relaxed_durability_requested(relaxed, true));
        for value in [None, Some(OsStr::new("0")), Some(OsStr::new("true"))] {
            assert!(!relaxed_durability_requested(value, true), "{value:?}");
        }
    }

    /// The child inherits the variable as any `orbit` process a test spawns
    /// does, and a debug build skips its syncs until a test opts back in.
    #[test]
    fn a_spawned_test_process_inherits_relaxed_syncs_until_it_opts_back_in() {
        let relaxed = if cfg!(debug_assertions) {
            "relaxed"
        } else {
            "durable"
        };
        for (value, expect) in [
            (Some("1"), relaxed),
            (Some("0"), "durable"),
            (None, "durable"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", CHILD, "--ignored", "--nocapture"])
                .env(EXPECT, expect);
            match value {
                Some(value) => command.env(RELAXED_DURABILITY_ENV, value),
                None => command.env_remove(RELAXED_DURABILITY_ENV),
            };
            let output = run_child_test(&mut command, CHILD, dir.path());
            assert_child_test_passed(CHILD, output.status, &output.stdout, &output.stderr);
        }
    }

    #[test]
    #[ignore = "re-executed by the relaxed-durability regression"]
    fn sync_probe_child() {
        let relaxed = std::env::var(EXPECT).unwrap() == "relaxed";
        let (_reader, writer) = std::io::pipe().unwrap();
        let pipe = File::from(OwnedFd::from(writer));
        let assert_syncs = |relaxed: bool| {
            assert_eq!(
                sync_file(&pipe).is_ok(),
                relaxed,
                "sync_file, relaxed={relaxed}"
            );
            assert_eq!(
                sync_file_data(&pipe).is_ok(),
                relaxed,
                "sync_file_data, relaxed={relaxed}"
            );
            let level = if relaxed { "OFF" } else { "FULL" };
            assert_eq!(sqlite_synchronous("FULL"), level);
        };

        assert_syncs(relaxed);
        let durable = require_durable_syncs();
        assert_syncs(false);
        drop(durable);
        assert_syncs(relaxed);
    }
}

use chrono::DateTime;
use chrono::Utc;
use counting_allocator::ALLOCATED;
use fs2::FileExt;
use orbit_common::ClaimRefusalKind;
use orbit_common::OrbitError;
use orbit_common::StorageLayer;
use orbit_common::derive_cost_usd;
use orbit_common::fs::directory_lock::DirectoryLockOptions;
use orbit_common::fs::directory_lock::with_directory_lock_options;
use orbit_common::fs::generation::Access;
use orbit_common::fs::generation::CompatibilityIdentity;
use orbit_common::fs::generation::FeatureFloor;
use orbit_common::fs::generation::GenerationGuard;
use orbit_common::fs::generation::GenerationUpdate;
use orbit_common::fs::generation::HandoverCandidate;
use orbit_common::fs::generation::LedgerCompatibility;
use orbit_common::fs::generation::Participant;
use orbit_common::fs::generation::ParticipantRequest;
use orbit_common::fs::generation::ParticipantRole;
use orbit_common::fs::generation::RESUME_MCP_STDIO;
use orbit_common::fs::generation::pending_switch;
use orbit_common::fs::git::run_git;
#[cfg(unix)]
use orbit_common::fs::git::run_git_within;
use orbit_common::fs::io::FileLockOptions;
use orbit_common::fs::io::with_exclusive_file_lock_options;
use orbit_common::fs::io::with_shared_file_lock_options;
use orbit_common::fs::io::write_new_private_text;
use orbit_common::fs::lock_wait::measure;
use orbit_common::fs::lock_wait::record;
use orbit_common::fs::reverse_lines::REVERSE_READ_BLOCK;
use orbit_common::fs::reverse_lines::ReverseLines;
use orbit_common::observability::logging;
#[cfg(unix)]
use orbit_common::process::OwnedChild;
use orbit_common::process::build_budget::read_waits;
#[cfg(unix)]
use orbit_common::process::identity::KernelLiveness;
#[cfg(unix)]
use orbit_common::process::identity::probe_process_group_liveness;
#[cfg(unix)]
use orbit_common::process::run_bounded;
#[cfg(unix)]
use orbit_common::process::shell::quote_posix_arg;
#[cfg(unix)]
use orbit_common::process::supervisor::CaptureSink;
#[cfg(unix)]
use orbit_common::process::supervisor::Periodic;
#[cfg(unix)]
use orbit_common::process::supervisor::ProcessStopCause;
#[cfg(unix)]
use orbit_common::process::supervisor::Stdin;
#[cfg(unix)]
use orbit_common::process::supervisor::Stream;
#[cfg(unix)]
use orbit_common::process::supervisor::SupervisedChild;
#[cfg(unix)]
use orbit_common::process::supervisor::Supervision;
#[cfg(unix)]
use orbit_common::process::supervisor::Termination;
#[cfg(unix)]
use orbit_common::storage::blob_store::BlobStore;
#[cfg(unix)]
use orbit_common::storage::blob_sweep;
#[cfg(unix)]
use orbit_common::storage::blob_sweep::AuditBlobRoot;
#[cfg(unix)]
use orbit_common::storage::blob_sweep::SweepEntry;
#[cfg(unix)]
use orbit_common::storage::blob_sweep::SweepOutcome;
use orbit_common::test_env;
use orbit_common::test_env::FixtureProgress;
use orbit_common::test_env::INHERITED_AUTHORITY_ENV;
use orbit_common::test_env::MANAGED_RUN_ENV;
use orbit_common::test_env::SCRUBBED_MARKER_ENV;
use orbit_common::test_env::assert_child_test_exists;
use orbit_common::test_env::assert_child_test_passed;
use orbit_common::test_env::clear_inherited_authority;
use orbit_common::test_env::run_child_test;
use orbit_types::task::ArtifactManifestV2;
use orbit_types::task::TaskCommentRowV2;
use orbit_types::task::TaskEnvelopeV2;
use orbit_types::task::TaskEventRowV2;
use orbit_types::telemetry::TokenUsage;
use serde_json::Value;
use serde_json::json;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs as std_fs;
use std::fs::File;
use std::fs::FileTimes;
use std::fs::OpenOptions;
use std::io;
#[cfg(unix)]
use std::io::BufRead;
use std::io::Cursor;
use std::io::ErrorKind;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::path::PathBuf;
#[cfg(unix)]
use std::process::Child;
use std::process::Command;
#[cfg(unix)]
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Barrier;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::mpsc::sync_channel;
#[cfg(unix)]
use std::thread;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use tempfile::TempDir;

orbit_common::isolate_test_process!();

#[cfg(unix)]
include!("audit_blob_sweep.rs");
include!("build_budget.rs");
include!("directory_lock.rs");
include!("error_classification.rs");
include!("file_lock_priority.rs");
include!("generation_admission.rs");
include!("generation_participation.rs");
include!("git_argv.rs");
#[cfg(unix)]
include!("git_deadline.rs");
include!("jsonl_exit_flush.rs");
#[cfg(all(target_os = "linux", debug_assertions))]
include!("jsonl_agent_feed.rs");
include!("lock_wait.rs");
include!("new_private_file.rs");
include!("pricing.rs");
#[cfg(unix)]
include!("process_group_liveness.rs");
include!("process_supervisor.rs");
#[cfg(unix)]
include!("process_supervisor_periodic.rs");
include!("reverse_lines.rs");
include!("task_artifacts_v2.rs");
include!("test_env.rs");

fn json_string_body(value: &str) -> String {
    let encoded = serde_json::to_string(value).expect("json string");
    encoded
        .strip_prefix('"')
        .and_then(|body| body.strip_suffix('"'))
        .expect("json quotes")
        .to_string()
}

/// `value` as the body of a Rust `Debug` string, without its quotes.
fn debug_string_body(value: &str) -> String {
    let encoded = format!("{value:?}");
    encoded
        .strip_prefix('"')
        .and_then(|body| body.strip_suffix('"'))
        .expect("debug quotes")
        .to_string()
}
