use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

pub const WAIT: Duration = Duration::from_secs(10);
const CHILD_MARKER: &str = "PROVIDER_INVOCATION_CHILD";

pub const HOSTILE_ENV: &[&str] = &[
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "GEMINI_API_KEY",
    "GH_TOKEN",
    "DATABASE_URL",
    "ORBIT_OPERATOR",
    "ORBIT_WORKSPACE_CLAIM_TOKEN",
    "ORBIT_UNKNOWN_PRIVILEGE",
];

pub fn scratch() -> TempDir {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.orbit/tmp");
    fs::create_dir_all(&root).expect("create fixture scratch root");
    tempfile::Builder::new()
        .prefix("provider-invocation-")
        .tempdir_in(root)
        .expect("isolated fixture directory")
}

/// Re-exec an exact case with disposable home and synthetic ambient secrets.
/// No process-global environment is mutated in the parallel libtest parent.
pub fn isolated(name: &str, run: impl FnOnce()) {
    if std::env::var(CHILD_MARKER).as_deref() == Ok(name) {
        run();
        return;
    }
    let fixture = scratch();
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").expect("PATH"))
        .env("HOME", fixture.path())
        .env("USERPROFILE", fixture.path())
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env(CHILD_MARKER, name)
        .env("ORBIT_RUN_ID", "fixture-run")
        .env("COPILOT_HOME", fixture.path())
        .env("EXPLICIT_PROVIDER_SETTING", "opted-in")
        .args(["--exact", name, "--nocapture", "--test-threads=1"]);
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    // Stamp synthetic context only after ambient authority has been cleared.
    command.env("ORBIT_RUN_ID", "fixture-run");
    for key in HOSTILE_ENV {
        command.env(key, format!("synthetic-{key}"));
    }
    // Use the shared load-tolerant hang guard for isolated provider cases.
    let output = orbit_common::test_env::run_child_test(&mut command, name, fixture.path());
    orbit_common::test_env::assert_child_test_passed(
        name,
        output.status,
        &output.stdout,
        &output.stderr,
    );
}

pub struct ChildGuard(pub Child);

impl ChildGuard {
    pub fn wait(&mut self, timeout: Duration) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.0.try_wait().expect("poll child") {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "child exceeded {timeout:?} ({})",
                orbit_common::test_env::host_load()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
