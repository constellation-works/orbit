mod loader;
mod materialize;
mod seed;
mod status;
mod sweep;
mod template;

/// Names the one test a child process was launched to run.
const ISOLATED_CHILD_ENV: &str = "ORBIT_TEST_ROUTINE_FIXTURE_CHILD";

/// Re-run the test `test` in `module` (pass `module_path!()`) in its own
/// process, and return `true` in the parent once that child passed.
///
/// These fixtures open Orbit runtimes, mint tasks, and submit jobs. Opening a
/// runtime restores the process's worker binding, so a suite launched from a
/// managed worker would refuse the fixture's fresh root, and inherited routing
/// would point writes at live state. The child drops that inherited authority
/// (including the worker-context requirement, which the shared list leaves to
/// the worker that sets it) and gets a disposable `HOME`/`USERPROFILE`, so the
/// result depends on the fixture alone. The parent requires positive evidence
/// that exactly the selected test ran, since a filter that matches nothing
/// also exits successfully.
fn isolated_child(module: &str, test: &str) -> bool {
    let module = module
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(module);
    let exact_test = format!("{module}::{test}");
    if std::env::var(ISOLATED_CHILD_ENV).ok().as_deref() == Some(exact_test.as_str()) {
        return false;
    }
    let home = tempfile::tempdir().expect("isolated home");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = command
        .args(["--exact", &exact_test, "--nocapture"])
        .env_remove("ORBIT_WORKER_CONTEXT_REQUIRED")
        .env(ISOLATED_CHILD_ENV, &exact_test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .output()
        .expect("run isolated routine fixture");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "isolated fixture {exact_test} failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains(&format!("test {exact_test} ... ok"))
            && stdout.contains("test result: ok. 1 passed;"),
        "the isolated child must execute exactly {exact_test}\nstdout:\n{stdout}"
    );
    true
}
