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
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
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

/// Share a bounded single-test child, never a row's authority or mutable state.
fn run_family(qualified: &str, module: &str, rows: &[(&str, fn())]) {
    if !run_isolated(qualified) {
        return;
    }
    struct RestoreCwd(std::path::PathBuf);
    impl Drop for RestoreCwd {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.0).expect("restore family cwd");
        }
    }
    for (name, row) in rows {
        let root =
            tempfile::tempdir_in(orbit_common::test_env::canonical_temp_dir()).expect("row root");
        let home = root.path().join("home");
        let tmp = root.path().join("tmp");
        std::fs::create_dir(&home).expect("row home");
        std::fs::create_dir(&tmp).expect("row temporary directory");
        let marker = format!("{}::{name}", module.split_once("::").unwrap().1);
        let home_text = home.to_str().expect("UTF-8 fixture path");
        let tmp_text = tmp.to_str().expect("UTF-8 fixture path");
        // These rows do not hold their own ScopedEnv. Keep tests that mutate
        // ambient credentials outside the family to avoid nested env locks.
        let _env = orbit_common::test_env::scoped([
            ("ORBIT_ENGINE_ADMISSION_AUDIT_CHILD", Some(marker.as_str())),
            ("HOME", Some(home_text)),
            ("USERPROFILE", Some(home_text)),
            ("TMPDIR", Some(tmp_text)),
            ("XDG_CONFIG_HOME", Some(home_text)),
        ]);
        let _cwd = RestoreCwd(std::env::current_dir().expect("family cwd"));
        std::env::set_current_dir(root.path()).expect("row cwd");
        row();
    }
}
