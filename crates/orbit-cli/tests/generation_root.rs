#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Binary-level coverage for generation admission isolation.
//!
//! `--root` and isolated `HOME` must be able to first-create `.generation.lock`
//! when the process home Orbit root is present, unpinned, and not writable —
//! those invocations only pin their own resolved root. `orbit update` is the
//! exception: it replaces the host binary, so it admits against the
//! host-global root as well and refuses when that record cannot be written.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::fs::generation::{GenerationGuard, executable_generation};
use orbit_common::test_env;
use serde_json::Value;
use tempfile::tempdir;

const FOREIGN_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn orbit(work: &Path, home: &Path) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home);
    command
}

#[cfg(unix)]
struct ReadOnlyDir {
    path: PathBuf,
}

#[cfg(unix)]
impl ReadOnlyDir {
    fn enter(path: PathBuf) -> Self {
        fs::create_dir_all(&path).expect("create directory to freeze");
        chmod(&path, 0o555);
        Self { path }
    }
}

#[cfg(unix)]
impl Drop for ReadOnlyDir {
    fn drop(&mut self) {
        chmod(&self.path, 0o755);
    }
}

#[cfg(unix)]
fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)
        .unwrap_or_else(|error| panic!("metadata {}: {error}", path.display()))
        .permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)
        .unwrap_or_else(|error| panic!("chmod {}: {error}", path.display()));
}

fn assert_generation_record(root: &Path) {
    let record = fs::read_to_string(root.join(".generation.lock")).expect("generation lock");
    assert!(
        record.starts_with("1:") && record.trim_end().len() == 66,
        "expected a v1 digest record, got {record:?}"
    );
}

#[cfg(unix)]
#[test]
fn init_with_root_succeeds_when_home_orbit_is_readonly_and_unpinned() {
    let temp = tempdir().expect("fixture tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    let scratch = temp.path().join("scratch");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&work).expect("create work");
    let home_orbit = home.join(".orbit");
    let _frozen = ReadOnlyDir::enter(home_orbit.clone());

    let output = orbit(&work, &home)
        .args([
            "--root",
            scratch.to_str().expect("utf-8 scratch"),
            "init",
            "--non-interactive",
            "--host-name",
            "qa-root",
            "--task-prefix",
            "QXQA",
        ])
        .output()
        .expect("run --root init");
    assert!(
        output.status.success(),
        "--root init failed against a read-only home Orbit root\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_generation_record(&scratch);
    assert!(scratch.join("config.toml").is_file());
    assert!(!home_orbit.join(".generation.lock").exists());
    assert!(!home_orbit.join(".generation-admission.lock").exists());
}

#[test]
fn init_with_isolated_home_writes_generation_lock_under_home_orbit() {
    let temp = tempdir().expect("fixture tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&work).expect("create work");

    let output = orbit(&work, &home)
        .args([
            "init",
            "--non-interactive",
            "--host-name",
            "qa-home",
            "--task-prefix",
            "QXQA",
        ])
        .output()
        .expect("run isolated-home init");
    assert!(
        output.status.success(),
        "isolated HOME init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let home_orbit = home.join(".orbit");
    assert_generation_record(&home_orbit);
    assert!(home_orbit.join("config.toml").is_file());
}

/// `orbit update` replaces the host binary whatever `--root` says, so the
/// host-global authority has to be able to record the candidate. A `~/.orbit`
/// this process cannot write is refused by the probe, not discovered after
/// the executable has already been swapped.
#[cfg(unix)]
#[test]
fn preflight_with_root_refuses_an_unwritable_host_global_authority() {
    let temp = tempdir().expect("fixture tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    let scratch = temp.path().join("scratch");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&work).expect("create work");
    let home_orbit = home.join(".orbit");
    let _frozen = ReadOnlyDir::enter(home_orbit.clone());

    let output = orbit(&work, &home)
        .args([
            "--root",
            scratch.to_str().expect("utf-8 scratch"),
            "update",
            "--preflight",
            "--json",
        ])
        .output()
        .expect("run --root preflight");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("upgrade admission refused"), "{stderr}");
    assert!(
        stderr.contains(&home_orbit.display().to_string()),
        "the refusal must name the authority it could not record against: {stderr}"
    );
    assert!(!home_orbit.join(".generation.lock").exists());
    assert!(!home_orbit.join(".generation-admission.lock").exists());
}

/// A `--root` override stays isolated for state, and admission over a
/// writable host-global root still reports both authorities.
#[cfg(unix)]
#[test]
fn preflight_with_root_reports_both_authorities_when_home_orbit_is_writable() {
    let temp = tempdir().expect("fixture tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    let scratch = temp.path().join("scratch");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&work).expect("create work");
    let home_orbit = home.join(".orbit");
    fs::create_dir_all(&home_orbit).expect("create home orbit");

    let output = orbit(&work, &home)
        .args([
            "--root",
            scratch.to_str().expect("utf-8 scratch"),
            "update",
            "--preflight",
            "--json",
        ])
        .output()
        .expect("run --root preflight");
    assert!(
        output.status.success(),
        "--root preflight failed against a writable home Orbit root\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("preflight JSON");
    assert_eq!(report["admitted"], true);
    assert_eq!(report["reservation"], false);
    assert_eq!(report["contract"], "executable-generation-v1");
    assert_eq!(report["global_root"], scratch.to_string_lossy().as_ref());
    assert_eq!(
        report["admission_roots"][1],
        home_orbit.to_string_lossy().as_ref()
    );
    assert!(
        scratch.join(".generation.lock").is_file()
            || scratch.join(".generation-admission.lock").is_file(),
        "preflight should create coordination lock files under --root"
    );
}

#[test]
fn preflight_on_isolated_home_is_blocked_by_a_live_exclusive_holder() {
    let temp = tempdir().expect("fixture tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&work).expect("create work");
    let home_orbit = home.join(".orbit");
    fs::create_dir_all(&home_orbit).expect("create home orbit");
    let digest = executable_generation(Path::new(env!("CARGO_BIN_EXE_orbit"))).expect("digest");
    let _holder = GenerationGuard::acquire(&home_orbit, &digest).expect("live same-digest pin");

    let output = orbit(&work, &home)
        .args(["update", "--preflight", "--json"])
        .output()
        .expect("run preflight against live holder");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Orbit clients or commands are still running"),
        "{stderr}"
    );
}

#[test]
fn isolated_home_refuses_a_different_executable_digest_while_a_pin_is_held() {
    let temp = tempdir().expect("fixture tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&work).expect("create work");
    let home_orbit = home.join(".orbit");
    fs::create_dir_all(&home_orbit).expect("create home orbit");
    let _pin = GenerationGuard::acquire(&home_orbit, FOREIGN_DIGEST).expect("foreign pin");

    let output = orbit(&work, &home)
        .args([
            "init",
            "--non-interactive",
            "--host-name",
            "qa-digest",
            "--task-prefix",
            "QXQA",
        ])
        .output()
        .expect("run init against foreign pin");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("another executable generation is still running"),
        "{stderr}"
    );
}

#[test]
fn init_with_root_ignores_a_foreign_pin_on_home_orbit() {
    let temp = tempdir().expect("fixture tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    let scratch = temp.path().join("scratch");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&work).expect("create work");
    let home_orbit = home.join(".orbit");
    fs::create_dir_all(&home_orbit).expect("create home orbit");
    let _pin = GenerationGuard::acquire(&home_orbit, FOREIGN_DIGEST).expect("foreign pin");

    let output = orbit(&work, &home)
        .args([
            "--root",
            scratch.to_str().expect("utf-8 scratch"),
            "init",
            "--non-interactive",
            "--host-name",
            "qa-isolated",
            "--task-prefix",
            "QXQA",
        ])
        .output()
        .expect("run --root init against foreign home pin");
    assert!(
        output.status.success(),
        "--root init should isolate from a home pin\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_generation_record(&scratch);
    let home_record = fs::read_to_string(home_orbit.join(".generation.lock")).expect("home pin");
    assert_eq!(home_record, format!("1:{FOREIGN_DIGEST}\n"));
}
