//! Probe lifetimes at the signed-release update boundary. Reuse the integrity
//! fixture without adding another responsibility to its large parent module.

use super::*;

#[cfg(target_os = "linux")]
#[test]
fn version_probe_retries_busy_executables_and_preserves_the_spawn_error() {
    let root = tempfile::tempdir().expect("probe root");
    let executable = root.path().join("candidate");
    fs::write(&executable, "#!/bin/sh\necho 'orbit 99.0.0'\n").expect("candidate");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).expect("executable");
    let writer = OpenOptions::new()
        .write(true)
        .open(&executable)
        .expect("writer");
    // A held write descriptor deterministically makes every exec fail with
    // ETXTBSY. The retry window must be exhausted rather than refusing the
    // first attempt, and the eventual error must remain the OS spawn error.
    let started = Instant::now();
    let error = orbit_cmd::update::converge::probe_version(&executable).expect_err("busy");
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "busy retry window was skipped"
    );
    assert!(
        error
            .to_string()
            .contains(&format!("os error {}", libc::ETXTBSY)),
        "{error}"
    );
    drop(writer);
    assert_eq!(
        orbit_cmd::update::converge::probe_version(&executable).expect("released executable"),
        "99.0.0"
    );
}
