use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use orbit_common::fs::file_lock::FileLockOptions;
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
        let raw = Command::new("git")
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
    let mut command = Command::new("git");
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

// Fault injection: the stalled remote is an ssh shim that never answers, so the
// fetch only ends when the holder's own deadline kills it. The scaled limits
// keep the production relation (hold limit below the waiter's wait); the
// relation of the real constants is pinned by const assertions beside them.
#[test]
fn stalled_delivery_fetch_releases_the_lock_before_a_waiter_times_out() {
    let root = tempfile::tempdir_in(canonical_temp_dir()).unwrap();
    let repo = root.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let started = root.path().join("fetch-started");
    let ssh = root.path().join("ssh");
    fs::write(
        &ssh,
        format!(
            "#!/bin/sh\necho started > '{}'\nexec sleep 600\n",
            started.display()
        ),
    )
    .unwrap();
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

    let hold_limit = Duration::from_millis(1500);
    let waiter_wait = Duration::from_secs(4);
    let lock_target = git_fetch_lock_target(&git_common_dir(&repo).unwrap());

    let (done, holder_done) = mpsc::channel();
    let holder_repo = repo.clone();
    let holder_started = Instant::now();
    let holder = thread::spawn(move || {
        // A raised activity budget must not lengthen the hold.
        let _budget = GitTimeoutBudgetGuard::install(GitTimeoutBudget {
            fetch_ms: GitTimeoutBudget::MAX_MS,
            ..GitTimeoutBudget::DEFAULT
        });
        let result = fetch_remote_base_within(&holder_repo, "main", hold_limit);
        done.send(holder_started.elapsed()).unwrap();
        result
    });

    let give_up = Instant::now() + Duration::from_secs(20);
    while !started.exists() {
        assert!(Instant::now() < give_up, "the stalled fetch never started");
        thread::sleep(Duration::from_millis(20));
    }

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

    let held = holder_done.recv().unwrap();
    assert!(
        held < hold_limit + Duration::from_secs(2),
        "the holder kept the lock {held:?} against a {hold_limit:?} limit"
    );
    let error = holder.join().unwrap().expect_err("a stalled fetch fails");
    let error = error.to_string();
    assert!(
        error.contains(TRANSIENT_FAILURE_MARKER),
        "a stalled remote stays a transient failure: {error}"
    );
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
