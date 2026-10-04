#![allow(missing_docs)]

mod add;
mod blocked_recovery;
mod contention;
mod final_recovery;
mod lifecycle;
mod paths;
mod transitions;

use std::process::Command;

use crate::OrbitRuntime;
use tempfile::tempdir;

/// Names the one test a re-executed child of this test binary runs in-process.
const ISOLATED_TEST_ENV: &str = "ORBIT_TEST_TASK_FIXTURE_CHILD";

/// Run the calling test's body in a child of this test binary.
///
/// A fixture that builds an Orbit runtime reads ambient authority from the
/// process environment. Inherited from a managed run, that authority can route
/// writes to the live workspace or, with `ORBIT_WORKER_CONTEXT_REQUIRED`, fail
/// runtime construction before the behavior under test runs; a temporary root
/// is not process isolation. The child starts with that authority cleared and
/// a disposable `HOME`, `USERPROFILE` and working directory.
///
/// Returns `true` inside the child, where the caller runs its body, and
/// `false` in the parent once the child ran exactly that test and passed.
pub(super) fn enter_isolated_child(module: &str, test: &str) -> bool {
    let module = module
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(module);
    let exact_test = format!("{module}::{test}");
    if std::env::var_os(ISOLATED_TEST_ENV).is_some_and(|name| name == exact_test.as_str()) {
        return true;
    }

    let home = tempdir().expect("isolated fixture home");
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = command
        .args(["--exact", &exact_test, "--nocapture", "--test-threads=1"])
        .env_remove("ORBIT_WORKER_CONTEXT_REQUIRED")
        .env(ISOLATED_TEST_ENV, &exact_test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .output()
        .expect("run isolated fixture");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "isolated `{exact_test}` failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("test result: ok. 1 passed;"),
        "the isolated child must run `{exact_test}` itself, not filter it out:\n{stdout}"
    );
    false
}

/// Refuse a runtime outside [`enter_isolated_child`], so a new fixture test
/// cannot silently run with the launching process's authority.
pub(super) fn assert_isolated_child() {
    assert!(
        std::env::var_os(ISOLATED_TEST_ENV).is_some(),
        "mutable task fixtures must run through `enter_isolated_child`"
    );
}

pub(super) fn test_runtime() -> (tempfile::TempDir, OrbitRuntime) {
    assert_isolated_child();
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    std::fs::write(
        workspace_root.join("config.toml"),
        r#"
[workflow]
default_crew = "implementer"

[crews.implementer]
model = "implementer-model"
provider = "codex"
backend = "cli"

[crews.orchestration]
model = "orchestration-model"
provider = "codex"
backend = "cli"
"#,
    )
    .expect("write crew config");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime)
}
