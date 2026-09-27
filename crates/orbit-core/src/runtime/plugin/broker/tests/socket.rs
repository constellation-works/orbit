use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use tempfile::tempdir;

use super::super::socket::{BROKER_DIR, RunSocketDir, SOCKET_NAME, sweep_orphaned};

const ORPHAN_HELPER: &str = "runtime::plugin::broker::tests::socket::leave_an_orphaned_run_dir";
const ORPHAN_ROOT_ENV: &str = "ORBIT_BROKER_TEST_ORPHAN_ROOT";

fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

#[test]
fn a_run_directory_is_private_to_the_host_and_removed_whole() {
    let root = tempdir().expect("global root");
    let global = root.path().canonicalize().expect("canonical root");

    let run = RunSocketDir::create(&global).expect("create run dir");

    let broker_root = global.join(BROKER_DIR);
    assert_eq!(run.socket_path().parent(), Some(run.dir()));
    assert_eq!(run.dir().parent(), Some(broker_root.as_path()));
    assert_eq!(run.socket_path().file_name(), Some(SOCKET_NAME.as_ref()));
    let token = run.dir().file_name().expect("token").to_string_lossy();
    assert!(
        token.len() == 16 && token.chars().all(|c| c.is_ascii_hexdigit()),
        "token {token} must be 16 hex characters"
    );
    assert_eq!(mode(&broker_root), 0o700);
    assert_eq!(mode(run.dir()), 0o700);

    run.remove().expect("remove run dir");
    assert!(!run.dir().exists());
    assert!(broker_root.is_dir(), "the shared broker root stays");
}

#[test]
fn two_runs_never_share_a_directory() {
    let root = tempdir().expect("global root");

    let first = RunSocketDir::create(root.path()).expect("first run");
    let second = RunSocketDir::create(root.path()).expect("second run");

    assert_ne!(first.dir(), second.dir());
}

#[test]
fn a_symlinked_broker_directory_is_refused() {
    let root = tempdir().expect("global root");
    let elsewhere = tempdir().expect("link target");
    fs::create_dir(root.path().join("state")).expect("state");
    std::os::unix::fs::symlink(elsewhere.path(), root.path().join(BROKER_DIR))
        .expect("plant symlink");

    let error = RunSocketDir::create(root.path()).expect_err("symlinked broker root");

    assert!(error.to_string().contains("symlink"), "{error}");
    assert_eq!(
        fs::read_dir(elsewhere.path()).expect("target").count(),
        0,
        "nothing may be created through the link"
    );
}

#[test]
fn a_symlinked_state_directory_is_refused() {
    let root = tempdir().expect("global root");
    let elsewhere = tempdir().expect("link target");
    std::os::unix::fs::symlink(elsewhere.path(), root.path().join("state")).expect("plant symlink");

    let error = RunSocketDir::create(root.path()).expect_err("symlinked state");

    assert!(error.to_string().contains("symlink"), "{error}");
    assert!(!elsewhere.path().join("plugin-broker").exists());
}

#[test]
fn a_socket_path_longer_than_sun_path_is_refused_before_anything_is_created() {
    let root = tempdir().expect("scratch");
    let global = root.path().join("g".repeat(120));
    fs::create_dir(&global).expect("long global root");

    let error = RunSocketDir::create(&global).expect_err("over-long socket path");

    assert!(error.to_string().contains("sun_path"), "{error}");
    assert!(
        !global.join("state").exists(),
        "an unusable broker must leave no directory behind"
    );
}

#[test]
fn a_relative_global_root_is_refused() {
    let error = RunSocketDir::create(Path::new("relative/root")).expect_err("relative root");

    assert!(error.to_string().contains("absolute"), "{error}");
}

/// Not a test on its own: the child half of the orphan sweep test. It creates
/// a run directory and exits without removing it, as a killed worker would.
#[test]
#[ignore = "child half of sweep_removes_only_directories_whose_owner_is_gone"]
fn leave_an_orphaned_run_dir() {
    let Some(root) = std::env::var_os(ORPHAN_ROOT_ENV) else {
        return;
    };
    // A run directory is removed only by an explicit `remove`, which a
    // killed worker never reaches.
    RunSocketDir::create(Path::new(&root)).expect("create orphan");
}

#[test]
fn sweep_removes_only_directories_whose_owner_is_gone() {
    let root = tempdir().expect("global root");
    let live = RunSocketDir::create(root.path()).expect("live run");
    let unowned = root.path().join(BROKER_DIR).join("unowned");
    fs::create_dir(&unowned).expect("directory without an owner file");

    let mut child = Command::new(std::env::current_exe().expect("test binary"));
    child.args(["--ignored", "--exact", ORPHAN_HELPER, "--test-threads=1"]);
    orbit_common::test_env::clear_inherited_authority(|name| {
        child.env_remove(name);
    });
    let output = child
        .env(ORPHAN_ROOT_ENV, root.path())
        .output()
        .expect("run orphan helper");
    assert!(
        output.status.success(),
        "orphan helper failed: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        run_dir_names(root.path()).len(),
        3,
        "live, unowned and orphaned directories before the sweep"
    );

    sweep_orphaned(root.path());

    let mut expected = vec![
        live.dir().file_name().expect("live token").to_os_string(),
        unowned.file_name().expect("unowned name").to_os_string(),
    ];
    expected.sort();
    assert_eq!(
        run_dir_names(root.path()),
        expected,
        "only the orphan is removed"
    );
}

fn run_dir_names(global_root: &Path) -> Vec<std::ffi::OsString> {
    let mut names: Vec<_> = fs::read_dir(global_root.join(BROKER_DIR))
        .expect("broker root")
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
    names.sort();
    names
}
