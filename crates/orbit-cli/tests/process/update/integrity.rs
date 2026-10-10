//! `orbit update` integrity at the CLI boundary, against a fake release mirror.
//!
//! The installed executable is a copy of this test's `orbit` binary in a
//! managed directory. Release bytes are a shell stand-in signed by a throwaway
//! key the child is told to trust. Refusals leave that copy and a seeded
//! generation record untouched. Admission waits for an in-flight clock tick,
//! and a candidate that hangs while it is probed releases every authority.

use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

use orbit_cmd::update::converge::PROBE_TIMEOUT_ENV;
use orbit_common::fs::generation::{
    Access, GenerationGuard, Participant, ParticipantRole, QUIESCE_TIMEOUT_ENV, RESUME_MCP_STDIO,
    executable_generation,
};
use orbit_common::test_env;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::output_of;

const WAIT_SLICE: Duration = Duration::from_millis(20);

/// [`PROBE_TIMEOUT_ENV`] for every fixture command, in seconds. Each probe's
/// clock starts at spawn, and macOS holds the first exec of a new executable,
/// such as the staged release, until it has assessed the file. That wait
/// queues behind every other new image on the host, so on a loaded host a
/// release script that answers at once outlasted the 30 s default. A hang
/// guard, half the command's [`test_env::FIXTURE_STEP_DEADLINE`], so a probe
/// that truly hangs is still reported by the update rather than killed with it.
const PROBE_BOUND_SECS: u64 = 60;

/// The probe bound under which the hanging candidate is killed. Linux starts
/// the script at once. On macOS the probe's clock also runs while the host
/// assesses the new script (see [`PROBE_BOUND_SECS`]), and a probe killed in
/// that wait never starts, so it gets the full hang guard.
const HUNG_PROBE_BOUND_SECS: u64 = if cfg!(target_os = "macos") {
    PROBE_BOUND_SECS
} else {
    8
};

struct Install {
    _root: TempDir,
    home: PathBuf,
    cwd: PathBuf,
    mirror: PathBuf,
    executable: PathBuf,
    private_key: PathBuf,
    trust_file: PathBuf,
    generation: PathBuf,
    before: Vec<u8>,
    generation_before: Vec<u8>,
}

impl Install {
    fn new(revoked_at: Option<&str>) -> Self {
        let root = tempfile::tempdir().expect("install root");
        let home = root.path().join("home");
        let cwd = root.path().join("cwd");
        let mirror = root.path().join("mirror");
        let bin = root.path().join("bin");
        for directory in [&home, &cwd, &mirror, &bin] {
            fs::create_dir_all(directory).expect("fixture directory");
        }
        crate::git_repo::seal_lookup_boundary(&cwd);
        let executable = super::install_test_binary(&bin);
        let before = fs::read(&executable).expect("installed bytes");
        let (private_key, public_key) = generate_keypair(root.path());
        let trust_file = root.path().join("trusted-release-keys.txt");
        write_trust(&trust_file, &public_key, revoked_at);
        let generation = home.join(".orbit").join(".generation.lock");
        fs::create_dir_all(generation.parent().expect("generation parent"))
            .expect("generation dir");
        let generation_before = format!("1:{}\n", "ab".repeat(32));
        fs::write(&generation, &generation_before).expect("seed generation");
        Self {
            _root: root,
            home,
            cwd,
            mirror,
            executable,
            private_key,
            trust_file,
            generation,
            before,
            generation_before: generation_before.into_bytes(),
        }
    }

    fn command(&self) -> assert_cmd::Command {
        let mut command = assert_cmd::Command::new(&self.executable);
        apply_env(&mut command, self);
        command.timeout(test_env::FIXTURE_STEP_DEADLINE);
        command
    }

    fn std_command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        apply_std_env(&mut command, self);
        command
    }

    /// Publish `version`. `archive` is omitted when the case must fail before download.
    fn publish(&self, version: &str, archive: Option<&[u8]>, sign_manifest: bool) {
        let directory = self.mirror.join(format!("v{version}"));
        fs::create_dir_all(&directory).expect("release directory");
        let asset = release_asset();
        let bytes = archive.unwrap_or_default();
        if let Some(archive) = archive {
            fs::write(directory.join(asset), archive).expect("write archive");
        }
        let digest = sha256_hex(bytes);
        let manifest = format!("{digest}  {asset}\n");
        let signed = if sign_manifest {
            manifest.clone()
        } else {
            format!("{manifest}# not what was published\n")
        };
        fs::write(directory.join("orbit-checksums.txt"), &manifest).expect("write manifest");
        fs::write(
            directory.join("orbit-checksums.txt.sig"),
            sign(&self.private_key, signed.as_bytes()),
        )
        .expect("write signature");
        fs::write(
            self.mirror.join("latest-version.txt"),
            format!("{version}\n"),
        )
        .expect("write latest");
    }

    fn tamper_archive(&self, version: &str) {
        let path = self
            .mirror
            .join(format!("v{version}"))
            .join(release_asset());
        let mut archive = fs::read(&path).expect("read archive");
        archive.extend_from_slice(b"tampered");
        fs::write(path, archive).expect("write tampered archive");
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = self.command();
        command.args(args);
        bounded_output(&mut command, &format!("orbit {}", args.join(" ")))
    }

    fn assert_untouched(&self, label: &str) {
        assert_eq!(
            fs::read(&self.executable).expect("installed bytes"),
            self.before,
            "{label} replaced the installed binary"
        );
        assert_eq!(
            fs::read(&self.generation).expect("generation record"),
            self.generation_before,
            "{label} rewrote the generation record"
        );
        assert!(
            !staging_remains(self.executable.parent().expect("install dir")),
            "{label} left a staging file behind"
        );
    }

    fn assert_no_backup(&self, label: &str) {
        let backup = backup_path(&self.executable);
        assert!(
            !backup.exists(),
            "{label} wrote a backup at {}",
            backup.display()
        );
    }
}

/// Run `command`, which [`Install::command`] bounds by
/// [`test_env::FIXTURE_STEP_DEADLINE`]. A command killed at that hang guard
/// fails naming `what` and the host load, rather than as a refusal.
fn bounded_output(command: &mut assert_cmd::Command, what: &str) -> Output {
    let started = Instant::now();
    let output = output_of(command).unwrap_or_else(|error| panic!("run `{what}`: {error}"));
    test_env::assert_step_finished(what, started.elapsed(), &output.status);
    output
}

/// Release a fixture participant only after `release` observes the updater's
/// rendezvous, then collect the bounded child and its output.
fn update_while(install: &Install, args: &[&str], release: impl FnOnce(&mut Child)) -> Output {
    let stdout = install._root.path().join("waiting-update.stdout");
    let stderr = install._root.path().join("waiting-update.stderr");
    let child = install
        .std_command()
        .env(QUIESCE_TIMEOUT_ENV, "60")
        .args(args)
        .stdout(File::create(&stdout).expect("update stdout"))
        .stderr(File::create(&stderr).expect("update stderr"))
        .spawn()
        .expect("spawn update");
    let mut child = ReapedChild { child: Some(child) };
    release(child.child.as_mut().expect("child"));
    let mut status = None;
    test_env::wait_until("the waiting update to finish", || {
        status = child
            .child
            .as_mut()
            .expect("child")
            .try_wait()
            .expect("poll update");
        status.is_some()
    });
    child.child = None;
    Output {
        status: status.expect("update finished"),
        stdout: fs::read(stdout).expect("update stdout"),
        stderr: fs::read(stderr).expect("update stderr"),
    }
}

fn apply_env(command: &mut assert_cmd::Command, install: &Install) {
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(&install.cwd)
        .env("HOME", &install.home)
        .env("USERPROFILE", &install.home)
        .env("ORBIT_UPDATE_RELEASE_DIR", &install.mirror)
        .env(
            "ORBIT_INSTALL_DIR",
            install.executable.parent().expect("install dir"),
        )
        .env("ORBIT_RELEASE_TRUSTED_KEYS_FILE", &install.trust_file)
        .env(
            "ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE",
            "1",
        )
        .env(PROBE_TIMEOUT_ENV, PROBE_BOUND_SECS.to_string())
        .env_remove("ORBIT_HOME")
        .env_remove("ORBIT_INSTALL_REPO")
        .env_remove("ORBIT_RELEASE_PUBLIC_KEY_FILE")
        .env_remove("ORBIT_RELEASE_PUBLIC_KEY_FILE_ACKNOWLEDGE_TRUST_CHANGE");
}

fn apply_std_env(command: &mut Command, install: &Install) {
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(&install.cwd)
        .env("HOME", &install.home)
        .env("USERPROFILE", &install.home)
        .env("ORBIT_UPDATE_RELEASE_DIR", &install.mirror)
        .env(
            "ORBIT_INSTALL_DIR",
            install.executable.parent().expect("install dir"),
        )
        .env("ORBIT_RELEASE_TRUSTED_KEYS_FILE", &install.trust_file)
        .env(
            "ORBIT_RELEASE_TRUSTED_KEYS_FILE_ACKNOWLEDGE_TRUST_CHANGE",
            "1",
        )
        .env(PROBE_TIMEOUT_ENV, PROBE_BOUND_SECS.to_string())
        .env_remove("ORBIT_HOME")
        .env_remove("ORBIT_INSTALL_REPO")
        .env_remove("ORBIT_RELEASE_PUBLIC_KEY_FILE")
        .env_remove("ORBIT_RELEASE_PUBLIC_KEY_FILE_ACKNOWLEDGE_TRUST_CHANGE");
}

fn assert_refused(output: &Output, label: &str, needle: &str) {
    assert_eq!(
        output.status.code(),
        Some(1),
        "{label} should refuse\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(needle),
        "{label} stderr did not contain {needle:?}: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "{label} wrote a payload on refusal: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

fn assert_not_downloaded(output: &Output, label: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains(release_asset()) && !stderr.contains("failed to read"),
        "{label} reached the archive download: {stderr}"
    );
}

#[test]
fn a_checksum_mismatch_never_reaches_the_install_path_and_staging_is_cleaned() {
    let install = Install::new(None);
    let archive = tar_gz(&candidate_script("99.0.0"));
    install.publish("99.0.0", Some(&archive), true);
    install.tamper_archive("99.0.0");

    let output = install.run(&["update"]);

    assert_refused(&output, "checksum mismatch", "checksum verification failed");
    install.assert_untouched("checksum mismatch");
    install.assert_no_backup("checksum mismatch");
}

#[test]
fn a_manifest_signed_over_different_bytes_is_rejected_before_download() {
    let install = Install::new(None);
    install.publish("99.0.0", None, false);

    let output = install.run(&["update"]);

    assert_refused(
        &output,
        "bad signature",
        "no trusted release signing key matched",
    );
    assert_not_downloaded(&output, "bad signature");
    install.assert_untouched("bad signature");
    install.assert_no_backup("bad signature");
}

#[test]
fn a_revoked_key_fails_closed_before_expiry() {
    let install = Install::new(Some("2020-01-01"));
    install.publish("99.0.0", None, true);

    let output = install.run(&["update"]);

    assert_refused(&output, "revoked key", "revoked 2020-01-01");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("expired"),
        "revoked key was reported as expired: {stderr}"
    );
    assert_not_downloaded(&output, "revoked key");
    install.assert_untouched("revoked key");
    install.assert_no_backup("revoked key");
}

#[test]
fn an_archive_with_extra_members_including_traversal_is_rejected() {
    let install = Install::new(None);
    let archive = tar_gz_named(&[
        ("orbit", candidate_script("99.0.0").as_slice()),
        ("../evil", b"payload".as_slice()),
    ]);
    install.publish("99.0.0", Some(&archive), true);

    let output = install.run(&["update"]);

    assert_refused(&output, "hostile archive", "must contain only");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("../evil"),
        "traversal member was normalized away: {stderr}"
    );
    install.assert_untouched("hostile archive");
    install.assert_no_backup("hostile archive");
}

#[test]
fn a_mislabeled_release_is_rejected_without_attempting_the_swap() {
    let install = Install::new(None);
    // Hold the original inode open so even a swap followed by rollback cannot
    // pass by reusing its inode number.
    let original = File::open(&install.executable).expect("installed executable");
    let archive = tar_gz(&candidate_script("0.0.1"));
    install.publish("99.0.0", Some(&archive), true);

    let output = install.run(&["update"]);

    assert_refused(&output, "version mismatch", "reports itself as 0.0.1");
    install.assert_untouched("version mismatch");
    install.assert_no_backup("version mismatch");
    assert_eq!(
        original.metadata().expect("original inode").ino(),
        fs::metadata(&install.executable)
            .expect("installed inode")
            .ino(),
        "a mislabeled release must never be swapped into the installed path, even temporarily"
    );
}

#[test]
fn a_downgrade_is_refused_unless_explicitly_requested() {
    let install = Install::new(None);
    let script = candidate_script("0.0.1");
    let archive = tar_gz(&script);
    install.publish("0.0.1", Some(&archive), true);

    let refused = install.run(&["update"]);
    assert_refused(&refused, "downgrade", "--allow-downgrade");
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("refusing to replace"), "{stderr}");
    assert_not_downloaded(&refused, "downgrade");
    install.assert_untouched("downgrade");
    install.assert_no_backup("downgrade");

    let allowed = install.run(&["update", "--allow-downgrade", "--json"]);
    assert!(
        allowed.status.success(),
        "explicit downgrade failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&allowed.stdout),
        String::from_utf8_lossy(&allowed.stderr)
    );
    let report: Value = serde_json::from_slice(&allowed.stdout).expect("downgrade report");
    assert_eq!(report["outcome"], "updated");
    assert_eq!(report["replaced"], true);
    assert_eq!(report["target_version"], "0.0.1");
    assert_eq!(
        fs::read(&install.executable).expect("downgraded bytes"),
        script,
        "explicit downgrade did not install the requested release"
    );
}

#[test]
fn a_second_concurrent_update_is_refused() {
    let install = Install::new(None);
    let script = candidate_script("99.0.0");
    let archive = tar_gz(&script);
    install.publish("99.0.0", Some(&archive), true);
    let lock_path = install
        .executable
        .parent()
        .expect("install dir")
        .join(".orbit-update.lock");
    let held = File::create(&lock_path).expect("create update lock");
    let rc = unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(rc, 0, "lock: {}", std::io::Error::last_os_error());

    let refused = install.run(&["update"]);
    assert_refused(
        &refused,
        "concurrent update",
        "another orbit update is already running",
    );
    install.assert_untouched("concurrent update");
    install.assert_no_backup("concurrent update");
    drop(held);

    let retried = install.run(&["update", "--json"]);
    assert!(
        retried.status.success(),
        "update after the lock was released failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&retried.stdout),
        String::from_utf8_lossy(&retried.stderr)
    );
    let report: Value = serde_json::from_slice(&retried.stdout).expect("retry report");
    assert_eq!(report["outcome"], "updated");
    assert_eq!(
        fs::read(&install.executable).expect("updated bytes"),
        script
    );
}

/// The running binary discovered `1.0.0` while blocked on the mirror. The
/// process has already recorded the install path (`current_exe` runs before
/// that read). Before the read completes, that directory entry is replaced
/// with a binary that reports `99.0.0` and the generation record already names
/// a newer digest. The locked re-probe must follow the live path and refuse
/// to put `1.0.0` in its place.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_stale_writer_and_an_older_binary_never_displace_a_newer_install() {
    let install = Install::new(None);
    let older = candidate_script("1.0.0");
    let newer = candidate_script("99.0.0");
    install.publish("1.0.0", Some(&tar_gz(&older)), true);
    fs::remove_file(install.mirror.join("latest-version.txt")).expect("remove regular latest");
    let latest = install.mirror.join("latest-version.txt");
    mkfifo(&latest);

    let stdout_path = install._root.path().join("stale.stdout");
    let stderr_path = install._root.path().join("stale.stderr");
    let child = install
        .std_command()
        .arg("update")
        .stdout(File::create(&stdout_path).expect("stdout"))
        .stderr(File::create(&stderr_path).expect("stderr"))
        .spawn()
        .expect("spawn stale update");
    let mut child = ReapedChild { child: Some(child) };

    // Opening the write end is the rendezvous with `latest_version`: the
    // child has finished `current_exe` and is blocked in the mirror read.
    // Only then is it safe to replace the directory entry that path names.
    // Rename, rather than truncating, so the still-mapped image is untouched.
    let mut writer = open_fifo_writer(
        &latest,
        child.child.as_mut().expect("child"),
        Instant::now() + test_env::FIXTURE_STEP_DEADLINE,
    );
    let preserved = install.executable.with_file_name("orbit.running");
    fs::rename(&install.executable, &preserved).expect("unlink running image");
    fs::write(&install.executable, &newer).expect("plant newer install");
    fs::set_permissions(&install.executable, fs::Permissions::from_mode(0o755))
        .expect("chmod newer install");
    writer
        .write_all(b"1.0.0\n")
        .expect("publish the stale latest version");
    drop(writer);

    let status = wait_exit(
        child.child.as_mut().expect("child"),
        Instant::now() + test_env::FIXTURE_STEP_DEADLINE,
    );
    child.child = None;
    let stderr = fs::read_to_string(&stderr_path).expect("stale stderr");
    let stdout = fs::read_to_string(&stdout_path).expect("stale stdout");
    assert_eq!(
        status.code(),
        Some(1),
        "stale writer should refuse\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("--allow-downgrade")
            && stderr.contains("99.0.0")
            && stderr.contains("1.0.0"),
        "stale writer stderr: {stderr}"
    );
    assert!(
        !stderr.contains("failed to read"),
        "stale writer installed instead of refusing: {stderr}"
    );
    assert!(stdout.is_empty(), "stale writer wrote a payload: {stdout}");
    assert_eq!(
        fs::read(&install.executable).expect("live install"),
        newer,
        "stale writer displaced the newer install"
    );
    assert_eq!(
        fs::read(&install.generation).expect("generation record"),
        install.generation_before,
        "stale writer rewrote the newer generation"
    );
    assert!(
        !staging_remains(install.executable.parent().expect("install dir")),
        "stale writer left a staging file"
    );
    let _ = child;
}

/// Hold the installation's host-global authority the way an in-flight
/// `orbit clock tick` does: registered as a clock tick, for as long as the
/// guard lives.
fn clock_tick(install: &Install) -> GenerationGuard {
    let identity = orbit_core::composition::compiled_compatibility();
    let digest = "c".repeat(64);
    let participant = Participant {
        digest: &digest,
        identity: &identity,
        role: ParticipantRole::Clock,
        access: Access::Write,
        handover: None,
        in_activity: false,
    };
    GenerationGuard::join(
        &install.home.join(".orbit"),
        &participant,
        Duration::ZERO,
        || Ok(0),
    )
    .expect("the clock tick joins")
}

#[test]
fn an_update_waits_for_an_in_flight_clock_tick_then_installs() {
    let install = Install::new(None);
    let script = candidate_script("99.0.0");
    install.publish("99.0.0", Some(&tar_gz(&script)), true);
    let tick = clock_tick(&install);
    let record = fs::read(&install.generation).expect("generation held by the tick");
    let output = update_while(&install, &["update", "--json"], |child| {
        // The updater holds admission exclusively while waiting for the tick.
        // Rendezvous with that lock instead of letting slow startup consume a
        // timed hold before the updater ever reaches admission.
        test_env::wait_until("the update to wait behind the clock tick", || {
            assert!(child.try_wait().expect("poll update").is_none());
            let Ok(admission) = File::open(install.home.join(".orbit/.generation-admission.lock"))
            else {
                return false;
            };
            // SAFETY: probe only this fixture's descriptor. Dropping it releases
            // a successful shared lock; EWOULDBLOCK identifies exclusive admission.
            if unsafe { libc::flock(admission.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
                return false;
            }
            let error = std::io::Error::last_os_error();
            assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock, "{error}");
            true
        });
        assert_eq!(
            fs::read(&install.executable).expect("installed"),
            install.before
        );
        assert_eq!(fs::read(&install.generation).expect("record"), record);
        drop(tick);
    });

    assert!(
        output.status.success(),
        "an update beside an in-flight tick must wait for it, not refuse\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("update report");
    assert_eq!(report["outcome"], "updated");
    assert_eq!(
        fs::read(&install.executable).expect("updated bytes"),
        script
    );
}

#[test]
fn an_update_refuses_a_clock_tick_that_outlasts_the_quiesce_bound() {
    let install = Install::new(None);
    install.publish("99.0.0", Some(&tar_gz(&candidate_script("99.0.0"))), true);
    let _tick = clock_tick(&install);

    let began = Instant::now();
    let mut command = install.command();
    command.env(QUIESCE_TIMEOUT_ENV, "1").arg("update");
    let output = bounded_output(&mut command, "orbit update beside an outlasting clock tick");

    assert_refused(
        &output,
        "outlasting tick",
        &format!("pid {} (clock tick, started", std::process::id()),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("after waiting 1s"), "{stderr}");
    assert!(stderr.contains("orbit clock pause"), "{stderr}");
    assert!(
        began.elapsed() >= Duration::from_secs(1),
        "the update refused before its bound"
    );
    assert_eq!(
        fs::read(&install.executable).expect("installed bytes"),
        install.before,
        "a refused update replaced the binary"
    );
    install.assert_no_backup("outlasting tick");
}

/// Hold the installation's host-global authority the way an idle stdio
/// `orbit mcp serve` does: registered to hand over with `mcp-stdio-v1`.
fn idle_session(install: &Install) -> GenerationGuard {
    let identity = orbit_core::composition::compiled_compatibility();
    let digest = "d".repeat(64);
    let participant = Participant {
        digest: &digest,
        identity: &identity,
        role: ParticipantRole::McpServe,
        access: Access::Write,
        handover: Some(RESUME_MCP_STDIO),
        in_activity: false,
    };
    GenerationGuard::join(
        &install.home.join(".orbit"),
        &participant,
        Duration::ZERO,
        || Ok(0),
    )
    .expect("the session joins")
}

/// A release update stages and probes the release before admission, so it
/// knows what the release resumes. A session it cannot resume refuses it
/// untouched; a session it can is admitted beside, named in the report, and
/// the release is pinned only once that session has released the replaced
/// generation, as its exec does when it hands over.
#[test]
fn a_release_update_admits_a_session_that_hands_over_and_pins_after_it_does() {
    let install = Install::new(None);
    let session = idle_session(&install);
    let record = fs::read(&install.generation).expect("generation record");

    install.publish("98.0.0", Some(&tar_gz(&candidate_script("98.0.0"))), true);
    let refused = install.run(&["update", "--version", "98.0.0"]);
    assert_refused(
        &refused,
        "a release that cannot resume the session",
        &format!("does not report the {RESUME_MCP_STDIO} resume capability"),
    );
    assert_eq!(
        fs::read(&install.executable).expect("installed"),
        install.before
    );
    assert_eq!(fs::read(&install.generation).expect("record"), record);
    assert!(!staging_remains(
        install.executable.parent().expect("install dir")
    ));
    install.assert_no_backup("a release that cannot resume the session");

    let script = resuming_candidate_script("99.0.0");
    install.publish("99.0.0", Some(&tar_gz(&script)), true);
    let output = update_while(
        &install,
        &["update", "--version", "99.0.0", "--json"],
        |child| {
            test_env::wait_until("the release to be installed", || {
                assert!(child.try_wait().expect("poll update").is_none());
                fs::read(&install.executable).ok().as_deref() == Some(script.as_slice())
            });
            // The session still holds the replaced generation after the rename:
            // it must not be pinned until that participant releases its lock.
            assert_eq!(fs::read(&install.generation).expect("record"), record);
            assert!(child.try_wait().expect("poll update").is_none());
            drop(session);
        },
    );

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("update report");
    assert_eq!(report["outcome"], "updated", "{report}");
    let handover = report["handover"].as_array().expect("handover list");
    assert_eq!(handover.len(), 1, "{report}");
    assert_eq!(handover[0]["pid"], std::process::id(), "{report}");
    assert_eq!(handover[0]["role"], "mcp_serve", "{report}");
    assert_eq!(handover[0]["resume"], RESUME_MCP_STDIO, "{report}");
    assert_eq!(fs::read(&install.executable).expect("installed"), script);
    assert_eq!(
        fs::read_to_string(&install.generation).expect("generation record"),
        format!(
            "1:{}\n",
            executable_generation(&install.executable).expect("installed digest")
        ),
        "the release is pinned once the session handed over"
    );
}

/// A release script reporting this build's admission contract, including the
/// resume capabilities a live session hands over with.
fn resuming_candidate_script(version: &str) -> Vec<u8> {
    let contract = Command::new(env!("CARGO_BIN_EXE_orbit"))
        .args(["update", "--contract", "--json"])
        .output()
        .expect("contract of this build");
    assert!(contract.status.success(), "{contract:?}");
    let contract = String::from_utf8(contract.stdout).expect("contract JSON");
    format!(
        "#!/bin/sh\n\
         if [ \"$1\" = --version ]; then echo 'orbit {version}'; exit 0; fi\n\
         if [ \"$1\" = update ] && [ \"$2\" = --contract ]; then \
         echo '{}'; exit 0; fi\n\
         exit 0\n",
        contract.trim()
    )
    .into_bytes()
}

/// A candidate whose `--version` never returns is killed at the probe
/// timeout. The staged candidate is probed before admission, so a hung probe
/// never holds an authority: commands run meanwhile, and nothing is held
/// afterwards.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_hanging_candidate_probe_times_out_without_holding_an_authority() {
    let install = Install::new(None);
    let probing = install._root.path().join("probe-started");
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = --version ]; then : > '{}'; exec sleep 600; fi\n\
         exit 0\n",
        probing.display()
    );
    install.publish("99.0.0", Some(&tar_gz(script.as_bytes())), true);

    let stderr_path = install._root.path().join("hung.stderr");
    let child = install
        .std_command()
        .env(PROBE_TIMEOUT_ENV, HUNG_PROBE_BOUND_SECS.to_string())
        .arg("update")
        .stdout(File::create(install._root.path().join("hung.stdout")).expect("stdout"))
        .stderr(File::create(&stderr_path).expect("stderr"))
        .spawn()
        .expect("spawn update");
    let mut child = ReapedChild { child: Some(child) };
    let deadline = Instant::now() + test_env::FIXTURE_STEP_DEADLINE;
    while !probing.exists() {
        assert_waiting(
            child.child.as_mut().expect("child"),
            deadline,
            "probe the candidate",
        );
        std::thread::sleep(WAIT_SLICE);
    }

    let during = install.run(&["update", "--preflight", "--json"]);
    assert!(
        during.status.success(),
        "a hung candidate probe held an authority: {}",
        String::from_utf8_lossy(&during.stderr)
    );

    let status = wait_exit(
        child.child.as_mut().expect("child"),
        Instant::now() + test_env::FIXTURE_STEP_DEADLINE,
    );
    child.child = None;
    let stderr = fs::read_to_string(&stderr_path).expect("update stderr");
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(&format!("did not finish within {HUNG_PROBE_BOUND_SECS}s")),
        "{stderr}"
    );
    assert!(stderr.contains("nothing was replaced"), "{stderr}");
    install.assert_untouched("hung candidate");
    install.assert_no_backup("hung candidate");

    let after = install.run(&["task", "list"]);
    let after_stderr = String::from_utf8_lossy(&after.stderr);
    assert!(
        !after_stderr.contains("upgrade admission refused"),
        "the timed-out update kept an authority: {after_stderr}"
    );
    let preflight = install.run(&["update", "--preflight", "--json"]);
    assert!(
        preflight.status.success(),
        "{}",
        String::from_utf8_lossy(&preflight.stderr)
    );
}

pub(super) struct ReapedChild {
    pub(super) child: Option<Child>,
}

impl Drop for ReapedChild {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Rendezvous with the mirror reader without a blocking opener thread or
/// Linux's non-portable read/write FIFO open. ENXIO means no reader yet.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn open_fifo_writer(path: &Path, child: &mut Child, deadline: Instant) -> File {
    loop {
        assert_waiting(child, deadline, "open the mirror");
        match OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => return file,
            Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                std::thread::sleep(WAIT_SLICE);
            }
            Err(error) => panic!("open mirror fifo: {error}"),
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn assert_waiting(child: &mut Child, deadline: Instant, what: &str) {
    if Instant::now() >= deadline {
        let _ = child.kill();
        panic!(
            "timed out waiting for the update to {what} ({})",
            test_env::host_load()
        );
    }
    if let Some(status) = child.try_wait().expect("poll update") {
        panic!("update exited before it could {what}: {status}");
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn wait_exit(child: &mut Child, deadline: Instant) -> std::process::ExitStatus {
    loop {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let status = child.wait().expect("reap timed-out update");
            panic!(
                "update did not finish before the deadline: {status} ({})",
                test_env::host_load()
            );
        }
        if let Some(status) = child.try_wait().expect("poll update") {
            return status;
        }
        std::thread::sleep(WAIT_SLICE);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn mkfifo(path: &Path) {
    let name = CString::new(path.as_os_str().as_bytes()).expect("fifo path");
    let rc = unsafe { libc::mkfifo(name.as_ptr(), 0o644) };
    assert_eq!(rc, 0, "mkfifo: {}", std::io::Error::last_os_error());
}

fn generate_keypair(dir: &Path) -> (PathBuf, PathBuf) {
    let private = dir.join("release-test.pem");
    let public = dir.join("release-test.pub");
    let generated = Command::new("openssl")
        .args([
            "genpkey",
            "-algorithm",
            "RSA",
            "-pkeyopt",
            "rsa_keygen_bits:2048",
            "-out",
        ])
        .arg(&private)
        .output()
        .expect("openssl genpkey");
    assert!(
        generated.status.success(),
        "openssl genpkey: {}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let exported = Command::new("openssl")
        .arg("pkey")
        .arg("-in")
        .arg(&private)
        .args(["-pubout", "-out"])
        .arg(&public)
        .output()
        .expect("openssl pkey");
    assert!(
        exported.status.success(),
        "openssl pkey: {}",
        String::from_utf8_lossy(&exported.stderr)
    );
    (private, public)
}

fn write_trust(path: &Path, public_key: &Path, revoked_at: Option<&str>) {
    let revoked = revoked_at.unwrap_or("");
    fs::write(
        path,
        format!(
            "orbit-test-key-1|2099-12-31|{revoked}|{}\n",
            public_key.display()
        ),
    )
    .expect("write trust file");
}

fn sign(private_key: &Path, message: &[u8]) -> Vec<u8> {
    let dir = tempfile::tempdir().expect("signature dir");
    let message_path = dir.path().join("message");
    let signature = dir.path().join("signature");
    fs::write(&message_path, message).expect("write message");
    let signed = Command::new("openssl")
        .args(["dgst", "-sha256", "-sign"])
        .arg(private_key)
        .arg("-out")
        .arg(&signature)
        .arg(&message_path)
        .output()
        .expect("openssl dgst");
    assert!(
        signed.status.success(),
        "openssl dgst: {}",
        String::from_utf8_lossy(&signed.stderr)
    );
    fs::read(signature).expect("read signature")
}

fn candidate_script(version: &str) -> Vec<u8> {
    format!(
        "#!/bin/sh\n\
         if [ \"$1\" = --version ]; then echo 'orbit {version}'; exit 0; fi\n\
         if [ \"$1\" = update ] && [ \"$2\" = --contract ]; then \
         echo '{{\"schema_version\":1,\"contract\":\"executable-generation-v1\"}}'; exit 0; fi\n\
         if [ \"$1\" = migrate ] && [ \"$2\" = --dry-run ]; then \
         echo '{{\"up_to_date\":true,\"schema\":{{\"current\":21,\"supported\":21}},\"layout\":{{\"current\":3,\"supported\":3}},\"forward_compatible\":{{\"read_only\":false}}}}'; \
         exit 0; fi\n\
         exit 0\n"
    )
    .into_bytes()
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Asset name for this build. Matches `channel::release_target_triple`.
fn release_asset() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "orbit-x86_64-unknown-linux-gnu.tar.gz",
        ("linux", "aarch64") => "orbit-aarch64-unknown-linux-gnu.tar.gz",
        ("macos", "aarch64") => "orbit-aarch64-apple-darwin.tar.gz",
        ("macos", "x86_64") => "orbit-x86_64-apple-darwin.tar.gz",
        (os, arch) => panic!("orbit publishes no release archive for {os}/{arch}"),
    }
}

fn tar_gz(body: &[u8]) -> Vec<u8> {
    tar_gz_named(&[("orbit", body)])
}

/// Member names are written into the ustar header directly. `set_path` refuses
/// `..`, and a hostile archive's traversal name is what the extractor must reject.
fn tar_gz_named(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, body) in members {
        let mut header = tar::Header::new_ustar();
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Regular);
        let raw = header.as_old_mut();
        let bytes = name.as_bytes();
        assert!(bytes.len() < raw.name.len(), "fixture member name too long");
        raw.name[..bytes.len()].copy_from_slice(bytes);
        header.set_cksum();
        builder.append(&header, *body).expect("append member");
    }
    let tar_bytes = builder.into_inner().expect("finish tar");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&tar_bytes).expect("gzip");
    encoder.finish().expect("finish gzip")
}

pub(super) fn staging_remains(directory: &Path) -> bool {
    fs::read_dir(directory)
        .expect("read install dir")
        .filter_map(Result::ok)
        .any(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with(".orbit-update-staged") || name.starts_with(".orbit-update-restore")
        })
}

pub(super) fn backup_path(executable: &Path) -> PathBuf {
    let mut name = executable.as_os_str().to_os_string();
    name.push(".previous");
    PathBuf::from(name)
}
