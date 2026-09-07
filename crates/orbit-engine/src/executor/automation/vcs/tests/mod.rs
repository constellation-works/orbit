#![allow(missing_docs)]

mod base_obsolescence;
mod git;
mod operations;

/// Isolate PATH in a child test process so real private VCS operations can run
/// a fake provider without changing the environment of concurrent Rust tests.
#[cfg(unix)]
pub(super) fn with_fake_gh(module: &str, test: &str, script: &str) -> bool {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let module = module
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(module);
    let exact_test = format!("{module}::{test}");
    if std::env::var("ORBIT_TEST_GH_CHILD").ok().as_deref() == Some(&exact_test) {
        return true;
    }
    let bin = tempfile::tempdir().expect("fake gh directory");
    let gh = bin.path().join("gh");
    fs::write(&gh, script).expect("write fake provider");
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).expect("executable provider");
    let mut paths = vec![bin.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", &exact_test, "--nocapture"])
        .env("ORBIT_TEST_GH_CHILD", &exact_test)
        .env("PATH", std::env::join_paths(paths).expect("provider PATH"))
        .output()
        .expect("isolated provider test");
    assert!(
        output.status.success(),
        "provider test failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}
