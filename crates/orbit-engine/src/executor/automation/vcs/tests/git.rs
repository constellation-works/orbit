use std::ffi::CString;
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use orbit_common::fs::file_lock::{FileLockOptions, try_acquire_exclusive_file_lock};
use orbit_common::fs::git::{git_common_dir, git_fetch_lock_target, is_git_ref_update_contention};
use orbit_common::fs::io::with_exclusive_file_lock_options;
use orbit_common::test_env::{assert_child_test_passed, canonical_temp_dir, run_child_test};
use orbit_types::workflow::TRANSIENT_FAILURE_MARKER;

use super::super::git::{
    GitTimeoutBudget, GitTimeoutBudgetGuard, fetch_remote_base_within, git_failure_error,
    git_output_raw, git_run, git_run_bytes,
};

// Fault injection: each shell alias fails silently, either by exiting or by
// signalling only its own Git parent. This reproduces the missing diagnostic
// without load or a process-global PATH shim (F2026-10-252).
#[test]
fn quiet_git_failures_keep_exit_status_and_signal_diagnostics() {
    let repo = tempfile::tempdir_in(canonical_temp_dir()).unwrap();
    git_ok(repo.path(), &["init", "-q", "--template=", "."]);
    for (alias, exit_code, signal, diagnostic) in [
        (
            "alias.quiet-failure=!exit 37",
            Some(37),
            None,
            "exit status: 37",
        ),
        (
            "alias.quiet-failure=!kill -KILL \"$PPID\"",
            None,
            Some(libc::SIGKILL),
            "signal: 9",
        ),
    ] {
        let args = ["-c", alias, "quiet-failure"];
        let raw = crate::git_test_support::command()
            .args(args)
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(!raw.status.success());
        assert_eq!(raw.status.code(), exit_code);
        assert_eq!(raw.status.signal(), signal);
        assert!(raw.stderr.is_empty(), "the Git fixture must fail silently");

        let outcome = git_run(repo.path(), &args).unwrap();
        assert!(!outcome.success && !outcome.timed_out);
        assert_eq!(outcome.exit_code, exit_code);
        let message =
            git_failure_error(repo.path(), &args, outcome.exit_code, &outcome.stderr).to_string();
        assert!(message.contains(diagnostic), "lost child status: {message}");

        let error = git_output_raw(repo.path(), &args).unwrap_err().to_string();
        assert!(error.contains(diagnostic), "lost child status: {error}");

        let bytes = git_run_bytes(repo.path(), &args, None).unwrap();
        assert!(!bytes.success && !bytes.timed_out);
        assert_eq!(bytes.exit_code, exit_code);
        let message =
            git_failure_error(repo.path(), &args, bytes.exit_code, &bytes.stderr).to_string();
        assert!(
            message.contains(diagnostic),
            "lost streaming child status: {message}"
        );
    }
}

fn git_ok(repo: &Path, args: &[&str]) {
    let mut command = crate::git_test_support::command();
    command.args(args).current_dir(repo);
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("spawn {command:?}: {error}"));
    assert!(
        output.status.success(),
        "{command:?} failed ({}):\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

// Both controls use real Git and a repository-local executable SSH shim.
fn ssh_fetch_fixture(root: &Path, script: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    let ssh = root.join("ssh");
    fs::write(&ssh, format!("#!/bin/sh\n{script}")).unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
    git_ok(&repo, &["init", "-q", "-b", "main"]);
    git_ok(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "ssh://fixture.invalid/owner/repository.git",
        ],
    );
    git_ok(&repo, &["config", "core.sshCommand", ssh.to_str().unwrap()]);
    git_ok(&repo, &["config", "ssh.variant", "ssh"]);

    (repo, ssh)
}

// The deadline includes Git/SSH startup. Observe the actual lock rather than
// waiting for SSH, so startup cannot consume the budget before the waiter runs.
// Real SSH startup/termination is checked independently by the positive control.
#[test]
fn stalled_delivery_fetch_releases_the_lock_before_a_waiter_times_out() {
    let root = tempfile::tempdir_in(canonical_temp_dir()).unwrap();
    let started = root.path().join("fetch-started");
    let (repo, ssh) = ssh_fetch_fixture(
        root.path(),
        &format!("echo started > '{}'\nexec sleep 600\n", started.display()),
    );
    let hold_limit = Duration::from_millis(1500);
    let waiter_wait = Duration::from_secs(4);
    let lock_target = git_fetch_lock_target(&git_common_dir(&repo).unwrap());

    thread::scope(|scope| {
        let (done, holder_done) = mpsc::sync_channel(1);
        let holder_repo = repo.clone();
        let holder_started = Instant::now();
        let holder = scope.spawn(move || {
            // A raised activity budget must not lengthen the hold.
            let _budget = GitTimeoutBudgetGuard::install(GitTimeoutBudget {
                fetch_ms: GitTimeoutBudget::MAX_MS,
                ..GitTimeoutBudget::DEFAULT
            });
            let result = fetch_remote_base_within(&holder_repo, "main", hold_limit);
            done.send((holder_started.elapsed(), format!("{result:?}")))
                .unwrap();
            result
        });

        let give_up = Instant::now() + Duration::from_secs(20);
        let lock_path = lock_target.with_file_name(".orbit-git-fetch.lock");
        loop {
            if try_acquire_exclusive_file_lock(&lock_path, "fixture contention probe")
                .unwrap()
                .is_none()
            {
                break;
            }
            assert!(
                Instant::now() < give_up,
                "the fetch lock was never contended"
            );
            match holder_done.recv_timeout(Duration::from_millis(20)) {
                Ok((elapsed, outcome)) => {
                    let joined = holder.join();
                    panic!(
                        "holder exited before the lock waiter after {elapsed:?}; \
                     phase=fetch_remote_base_within, ssh_started={}, outcome={outcome}, \
                     joined={joined:?}, repo={}, ssh={}",
                        started.exists(),
                        repo.display(),
                        ssh.display()
                    );
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let joined = holder.join();
                    let outcome = joined.as_ref().err().map(|payload| {
                        payload
                            .downcast_ref::<String>()
                            .map(String::as_str)
                            .or_else(|| payload.downcast_ref::<&str>().copied())
                            .unwrap_or("non-string panic payload")
                    });
                    panic!(
                        "holder panicked before the lock waiter after {:?}; \
                     phase=fetch_remote_base_within, ssh_started={}, outcome={outcome:?}, \
                     joined={joined:?}, repo={}, ssh={}",
                        holder_started.elapsed(),
                        started.exists(),
                        repo.display(),
                        ssh.display()
                    );
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }

        // The probe observed contention on the same dot-prefixed sibling acquired
        // by this real waiter, independently of whether the SSH shim has started.
        // A pilot- or final-recovery-style waiter, mid-hold, outwaits the holder.
        let waited = Instant::now();
        let acquired = with_exclusive_file_lock_options(
            &lock_target,
            "git fetch",
            FileLockOptions {
                timeout: waiter_wait,
                warn_after: waiter_wait,
                ..FileLockOptions::default()
            },
            || Ok::<_, std::io::Error>(waited.elapsed()),
        )
        .expect("a waiter must acquire the lock once the holder's deadline passes, not time out");
        assert!(acquired < waiter_wait, "waited {acquired:?}");

        let (held, outcome) = holder_done.recv().unwrap();
        assert!(
            held < hold_limit + Duration::from_secs(2),
            "the holder kept the lock {held:?} against a {hold_limit:?} limit"
        );
        let error = holder.join().unwrap().expect_err("a stalled fetch fails");
        assert!(
            matches!(error, orbit_common::OrbitError::ExecutionTimeout { .. }),
            "the fetch must end through the holder's deadline: {error}"
        );
        tracing::subscriber::with_default(
            tracing_subscriber::fmt()
                .with_test_writer()
                .without_time()
                .with_ansi(false)
                .finish(),
            || {
                tracing::info!(
                    ?held, ?acquired, %outcome,
                    ssh_started = started.exists(),
                    "fetch lock contention confirmed; holder released; waiter acquired"
                );
            },
        );
        let error = error.to_string();
        assert!(
            error.contains(TRANSIENT_FAILURE_MARKER),
            "a stalled remote stays a transient failure: {error}"
        );
    });
}

// A FIFO releases the shim explicitly, including when an assertion unwinds.
// Keeping both ends open makes release nonblocking even if startup failed.
struct SshRelease(fs::File);

impl Drop for SshRelease {
    fn drop(&mut self) {
        let _ = self.0.write_all(b"terminate\n");
    }
}

#[test]
fn real_ssh_fetch_starts_under_lock_and_reaps_before_release() {
    let root = tempfile::tempdir_in(canonical_temp_dir()).unwrap();
    let started = root.path().join("ssh-started");
    let release_path = root.path().join("ssh-release");
    let fifo = CString::new(release_path.as_os_str().as_bytes()).unwrap();
    // SAFETY: fifo is a live NUL-terminated path in this fixture's private root.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let release = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&release_path)
        .unwrap();
    let (repo, _ssh) = ssh_fetch_fixture(
        root.path(),
        &format!(
            "printf '%s %s\\n' \"$$\" \"$PPID\" > '{}.tmp'\n\
             mv '{}.tmp' '{}'\n\
             read -r command < '{}'\n\
             test \"$command\" = terminate || exit 38\n\
             echo 'Permission denied: controlled SSH termination' >&2\n\
             exit 37\n",
            started.display(),
            started.display(),
            started.display(),
            release_path.display(),
        ),
    );
    let lock_target = git_fetch_lock_target(&git_common_dir(&repo).unwrap());
    let lock_path = lock_target.with_file_name(".orbit-git-fetch.lock");
    let startup_limit = Duration::from_secs(20);
    let cleanup_limit = Duration::from_secs(4);

    thread::scope(|scope| {
        let (done, holder_done) = mpsc::sync_channel(1);
        let holder_repo = repo.clone();
        let holder = scope.spawn(move || {
            let result = fetch_remote_base_within(&holder_repo, "main", startup_limit);
            done.send(format!("{result:?}")).unwrap();
            result
        });
        // Drops before scope joins the holder, so even a failed assertion lets
        // an already-started (or subsequently starting) shim finish normally.
        let mut release = SshRelease(release);
        let give_up = Instant::now() + startup_limit;
        while !started.exists() {
            assert!(
                Instant::now() < give_up,
                "real Git never started the executable SSH shim within {startup_limit:?}"
            );
            match holder_done.recv_timeout(Duration::from_millis(20)) {
                Ok(outcome) => panic!(
                    "holder exited before SSH startup: outcome={outcome}, joined={:?}",
                    holder.join()
                ),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("holder panicked before SSH startup: {:?}", holder.join());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        let identities = fs::read_to_string(&started).unwrap();
        let pids: Vec<libc::pid_t> = identities
            .split_whitespace()
            .map(|pid| pid.parse().unwrap())
            .collect();
        let [ssh_pid, git_pid] = pids.as_slice() else {
            panic!("invalid SSH startup identities: {identities:?}");
        };
        assert!(*ssh_pid > 0 && *git_pid > 0);
        // SAFETY: getpgid only observes the live shim. The supervised Git child
        // leads its own group; this also verifies that $PPID names that child.
        assert_eq!(unsafe { libc::getpgid(*ssh_pid) }, *git_pid);
        assert!(
            try_acquire_exclusive_file_lock(&lock_path, "SSH startup contention probe")
                .unwrap()
                .is_none(),
            "real SSH must start while the fetch holder owns the actual lock"
        );
        assert!(
            matches!(holder_done.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "the fetch must remain active until the shim is released"
        );

        let terminating = Instant::now();
        release.0.write_all(b"terminate\n").unwrap();
        let outcome = holder_done
            .recv_timeout(cleanup_limit)
            .expect("controlled SSH termination must finish the fetch promptly");
        let error = holder.join().unwrap().expect_err("the shim refuses fetch");
        assert!(
            matches!(error, orbit_common::OrbitError::Execution(_)),
            "explicit release must finish before the startup deadline: {error}"
        );
        assert!(
            error.to_string().contains("controlled SSH termination"),
            "the shim must acknowledge the termination command: {error}"
        );
        // SAFETY: waitpid targets only the observed Git child, after its owner
        // returned. ECHILD proves the supervisor already reaped it. kill(0)
        // observes shim absence, including absence of an unreaped zombie.
        let waited = unsafe { libc::waitpid(*git_pid, std::ptr::null_mut(), libc::WNOHANG) };
        let wait_error = std::io::Error::last_os_error();
        assert_eq!(waited, -1, "Git must already be reaped: {outcome}");
        assert_eq!(wait_error.raw_os_error(), Some(libc::ECHILD));
        let alive = unsafe { libc::kill(*ssh_pid, 0) };
        let probe_error = std::io::Error::last_os_error();
        assert_eq!(alive, -1, "Git must reap its SSH child before returning");
        assert_eq!(probe_error.raw_os_error(), Some(libc::ESRCH));
        with_exclusive_file_lock_options(
            &lock_target,
            "git fetch",
            FileLockOptions {
                timeout: cleanup_limit.saturating_sub(terminating.elapsed()),
                warn_after: cleanup_limit,
                ..FileLockOptions::default()
            },
            || Ok::<_, std::io::Error>(()),
        )
        .expect("controlled SSH termination must release the real fetch lock");
        assert!(terminating.elapsed() < cleanup_limit);
    });
}

// Security invariant: the private host runner must fix Git's effective locale
// before stderr-based retry decisions. An isolated child supplies the parent
// locale without racing other tests; the public engine does not expose this runner.
#[test]
fn host_git_classifies_failures_in_the_c_locale() {
    const TEST: &str =
        "executor::automation::vcs::tests::git::host_git_classifies_failures_in_the_c_locale";
    const CHILD: &str = "ORBIT_TEST_HOST_GIT_LOCALE";
    if std::env::var_os(CHILD).is_some() {
        let repo = tempfile::tempdir_in(canonical_temp_dir()).unwrap();
        git_ok(repo.path(), &["init", "-q", "--template=", "."]);
        let args = [
            "-c",
            "alias.child-locale=!printf '%s\\n' \"$LC_ALL\" \"${LANGUAGE-unset}\"",
            "child-locale",
        ];
        let outcome = git_run(repo.path(), &args).unwrap();
        assert!(outcome.success, "{}", outcome.stderr);
        assert_eq!(outcome.stdout, "C\nunset\n");
        let bytes = git_run_bytes(repo.path(), &args, None).unwrap();
        assert!(bytes.success, "{}", bytes.stderr);
        assert_eq!(bytes.stdout, b"C\nunset\n");
        git_ok(
            repo.path(),
            &[
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@orbit.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "--no-verify",
                "-q",
                "-m",
                "base",
            ],
        );
        // Force contention directly rather than relying on host timing.
        fs::write(repo.path().join(".git/refs/heads/fixture.lock"), "held").unwrap();
        let failed = git_run(repo.path(), &["update-ref", "refs/heads/fixture", "HEAD"]).unwrap();
        assert!(!failed.success && !failed.timed_out);
        assert!(
            is_git_ref_update_contention(&failed.stderr),
            "{}",
            failed.stderr
        );
        return;
    }
    let output_dir = tempfile::tempdir_in(canonical_temp_dir()).unwrap();
    for locale in ["C", "de_DE.UTF-8"] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, "1")
            .env("LANG", locale)
            .env("LC_ALL", locale)
            .env("LANGUAGE", "de");
        let output = run_child_test(&mut child, TEST, output_dir.path());
        assert_child_test_passed(TEST, output.status, &output.stdout, &output.stderr);
    }
}
