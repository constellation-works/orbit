use std::fs;
use std::path::Path;
use std::process::Command;

pub(super) fn read_count(path: &Path) -> usize {
    fs::read_to_string(path)
        .expect("read gh call count")
        .trim()
        .parse()
        .expect("gh call count is numeric")
}

pub(super) fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, contents).expect("write gh stub");
    let permissions = fs::Permissions::from_mode(0o755);
    fs::set_permissions(path, permissions).expect("make gh stub executable");
}

pub(super) fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

// A scoped PATH guard protects only guarded writers, not sibling Git readers.
// Supply the stub directory on the child command so parallel libtest fixtures
// always resolve the real tools (F2026-10-275, F2026-10-276).
pub(super) fn with_stub_path(test: &str, run: impl FnOnce(&Path)) {
    with_stub_root(None, test, run);
}

// Like `with_stub_path`, but the root's name starts with `prefix`. A prefix with
// spaces or apostrophes exercises stubs that interpolate their paths into shell.
pub(super) fn with_named_stub_path(prefix: &str, test: &str, run: impl FnOnce(&Path)) {
    with_stub_root(Some(prefix), test, run);
}

fn with_stub_root(prefix: Option<&str>, test: &str, run: impl FnOnce(&Path)) {
    const MARKER: &str = "ORBIT_TEST_VCS_STUB_CHILD";
    const ROOT: &str = "ORBIT_TEST_VCS_STUB_ROOT";
    let test = test.split_once("::").expect("test has crate prefix").1;
    if std::env::var(MARKER).as_deref() == Ok(test) {
        let root = std::env::var_os(ROOT).expect("child fixture root");
        run(Path::new(&root));
        return;
    }

    let mut builder = tempfile::Builder::new();
    if let Some(prefix) = prefix {
        builder.prefix(prefix);
    }
    let root = builder
        .tempdir_in(orbit_common::test_env::canonical_temp_dir())
        .expect("temporary VCS fixture");
    let bin = root.path().join("bin");
    fs::create_dir(&bin).expect("create executable directory");
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(paths).expect("compose stub PATH");
    let mut child = Command::new(std::env::current_exe().expect("test executable"));
    orbit_common::test_env::clear_inherited_authority(|key| {
        child.env_remove(key);
    });
    child
        .args(["--exact", test, "--nocapture"])
        .env(MARKER, test)
        .env(ROOT, root.path())
        .env("PATH", path)
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .current_dir(root.path());
    let output = orbit_common::test_env::run_child_test(&mut child, test, root.path());
    orbit_common::test_env::assert_child_test_passed(
        test,
        output.status,
        &output.stdout,
        &output.stderr,
    );
}
