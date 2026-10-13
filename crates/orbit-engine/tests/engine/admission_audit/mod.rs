//! Existing audit cases at exported engine boundaries, with disposable child authority.
#[cfg(unix)]
mod cli_support;
mod commit;
mod dispatcher;
mod gc;
#[cfg(unix)]
mod inspection;
mod job_support;
#[cfg(unix)]
mod launcher;
#[cfg(unix)]
mod operations;
#[cfg(unix)]
mod orchestrator_env;
mod resume;
mod shell;
#[cfg(unix)]
mod trusted_host;

/// Return true only in the exact child that executes the fixture's body.
fn run_isolated(qualified: &str) -> bool {
    let test = qualified.split_once("::").expect("crate-qualified test").1;
    const MARKER: &str = "ORBIT_ENGINE_ADMISSION_AUDIT_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let root =
        tempfile::tempdir_in(orbit_common::test_env::canonical_temp_dir()).expect("isolated root");
    let home = root.path().join("home");
    std::fs::create_dir(&home).expect("isolated home");
    let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"));
    orbit_common::test_env::clear_inherited_authority(|key| {
        child.env_remove(key);
    });
    child
        .args(["--exact", test, "--nocapture"])
        .env(MARKER, test)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .current_dir(root.path());
    let output = orbit_common::test_env::run_child_test(&mut child, test, root.path());
    orbit_common::test_env::assert_child_test_passed(
        test,
        output.status,
        &output.stdout,
        &output.stderr,
    );
    false
}
