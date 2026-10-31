//! An `orbit` process registers as a generation participant once, however
//! many registered checkouts it opens [ORB-15674].
//!
//! Each registration stages, fsyncs and renames a record. Under drain load
//! `orbit routine list` registered once for the command and again for every
//! checkout it inspected, and the dashboard did the same for every
//! `/api/routines` computation, so routine status took tens of seconds. The
//! count is read from the registration's own debug event.
//!
//! Host-scoped reads also work without a cwd binding when joining a live
//! generation with a different executable digest.

#![allow(missing_docs)]
// Integration fixtures use expect/unwrap for concise failure diagnostics.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use orbit_common::test_env;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

use crate::bound_server::{Announcement, BoundServer};

const STARTUP_DEADLINE: Duration = Duration::from_secs(30);
/// The registration event's tracing target.
const REGISTRATION_TARGET: &str = "orbit.generation.participant";

/// A home with three registered workspaces, each seeding its routines.
struct Host {
    temp: TempDir,
    home: std::path::PathBuf,
}

impl Host {
    fn new() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home");
        let host = Self { temp, home };
        for (index, name) in ["alpha", "beta", "gamma"].into_iter().enumerate() {
            let repo = host.temp.path().join(name);
            crate::git_repo::init(&repo);
            if index == 0 {
                host.orbit(
                    &repo,
                    &[
                        "init",
                        "--non-interactive",
                        "--machine-name",
                        "participation-host",
                        "--task-prefix",
                        "GP",
                    ],
                );
            }
            host.orbit(&repo, &["workspace", "init", "--name", name]);
        }
        host
    }

    /// `orbit` with only the registration event logged to stderr.
    fn command(&self, cwd: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orbit"));
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("RUST_LOG", format!("off,{REGISTRATION_TARGET}=debug"))
            .env("NO_COLOR", "1");
        command
    }

    fn orbit(&self, cwd: &Path, args: &[&str]) {
        let output = self.command(cwd).args(args).output().expect("run orbit");
        assert!(
            output.status.success(),
            "orbit {args:?} failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn registrations(stderr: &str) -> usize {
    stderr
        .lines()
        .filter(|line| line.contains(REGISTRATION_TARGET))
        .count()
}

/// Routine sources: one per registered workspace whose routines were read.
fn sources(routines: &Value) -> Vec<String> {
    let mut sources = routines["routines"]
        .as_array()
        .expect("routine array")
        .iter()
        .filter_map(|routine| routine["source"].as_str().map(str::to_string))
        .collect::<Vec<_>>();
    sources.sort();
    sources.dedup();
    sources
}

#[test]
fn routine_list_registers_once_across_every_registered_checkout() {
    let host = Host::new();
    let output = host
        .command(host.temp.path())
        .args(["routine", "list", "--format", "json"])
        .output()
        .expect("run orbit routine list");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    let routines: Value = serde_json::from_slice(&output.stdout).expect("routine list JSON");
    assert_eq!(
        sources(&routines),
        ["alpha", "beta", "gamma"],
        "routine status opens every registered checkout: {routines}"
    );
    assert_eq!(
        registrations(&stderr),
        1,
        "one registration for the command, none per opened checkout: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn workspace_list_from_unregistered_cwd_joins_foreign_generation_without_writes() {
    if crate::isolated_test::run_in_child(
        "generation_participation::workspace_list_from_unregistered_cwd_joins_foreign_generation_without_writes",
    ) {
        return;
    }

    use orbit_common::fs::generation::{Access, GenerationGuard, Participant, ParticipantRole};

    let host = Host::new();
    // Registration alone does not initialize the runtime's databases. A live
    // dashboard would already have opened them before a foreign reader joins.
    host.orbit(&host.temp.path().join("alpha"), &["task", "list", "--json"]);
    let cwd = host.temp.path().join("unregistered");
    fs::create_dir(&cwd).expect("unregistered cwd");
    let root = host.home.join(".orbit");
    let identity = orbit_core::composition::compiled_compatibility();
    let digest = "a".repeat(64);
    let _record = GenerationGuard::acquire(&root, &digest).expect("record foreign digest");
    let _generation = GenerationGuard::join(
        &root,
        &Participant {
            digest: &digest,
            identity: &identity,
            role: ParticipantRole::Dashboard,
            access: Access::Write,
            handover: None,
            in_activity: false,
        },
        Duration::ZERO,
        || Ok(identity.store_schema.version),
    )
    .expect("hold live compatible foreign generation");
    let generation_before = fs::read(root.join(".generation.lock")).expect("generation record");
    let partitions = || {
        let mut entries = fs::read_dir(root.join("tasks/workspaces"))
            .expect("task partitions")
            .map(|entry| entry.expect("partition entry").file_name())
            .collect::<Vec<_>>();
        entries.sort();
        entries
    };
    let partitions_before = partitions();

    let output = host
        .command(&cwd)
        .args(["workspace", "list", "--json"])
        .output()
        .expect("run orbit workspace list");
    assert!(
        output.status.success(),
        "workspace list must read the host registry without a cwd binding: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let workspaces: Value = serde_json::from_slice(&output.stdout).expect("workspace list JSON");
    let mut names = workspaces
        .as_array()
        .expect("workspace array")
        .iter()
        .map(|workspace| workspace["name"].as_str().expect("workspace name"))
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["alpha", "beta", "gamma"]);
    assert!(
        !cwd.join(".orbit").exists(),
        "listing must not bind the cwd"
    );
    assert_eq!(
        partitions(),
        partitions_before,
        "listing must not mint a partition"
    );
    assert_eq!(
        fs::read(root.join(".generation.lock")).expect("generation record after listing"),
        generation_before,
        "listing must retain the live foreign generation"
    );
}

#[cfg(unix)]
#[test]
fn task_list_from_unregistered_cwd_does_not_return_unbound_tasks() {
    if crate::isolated_test::run_in_child(
        "generation_participation::task_list_from_unregistered_cwd_does_not_return_unbound_tasks",
    ) {
        return;
    }

    use orbit_common::fs::generation::{Access, GenerationGuard, Participant, ParticipantRole};

    let host = Host::new();
    let root = host.home.join(".orbit");
    let root_str = root.to_str().expect("utf8 root").to_string();
    let sentinel_title = "unbound sentinel ORB-19676";
    // A live dashboard would already have opened the databases before a
    // foreign reader joins, so seed them here as well.
    host.orbit(&host.temp.path().join("alpha"), &["task", "list", "--json"]);
    // Seed the shared unbound partition through the documented explicit-root
    // path: export one registered task, then import it into the synthetic
    // `ws_unbound-data-dir` workspace. The unbound workspace has no registry
    // row, so seed it first; import then lands there and explicit-root list
    // reads it back.
    let alpha = host.temp.path().join("alpha");
    {
        let index = root.join("tasks").join("index.sqlite");
        let conn = rusqlite::Connection::open(&index).expect("open task registry");
        conn.execute(
            "INSERT OR IGNORE INTO workspace_bindings \
             (workspace_id, slug, repo_fingerprint, created_at, updated_at) \
             VALUES (?1, ?2, NULL, ?3, ?3)",
            rusqlite::params![
                "ws_unbound-data-dir",
                "unbound-data-dir",
                "2026-01-01T00:00:00+00:00",
            ],
        )
        .expect("seed unbound workspace");
    }
    let add = host
        .command(&alpha)
        .args([
            "task",
            "add",
            "--title",
            sentinel_title,
            "--complexity",
            "low",
        ])
        .output()
        .expect("add sentinel to alpha");
    assert!(
        add.status.success(),
        "task add in alpha must succeed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    let archive = host.temp.path().join("sentinel.tar.zst");
    let archive_str = archive.to_str().expect("utf8 archive").to_string();
    let export = host
        .command(&alpha)
        .args(["task", "export", "-o", &archive_str])
        .output()
        .expect("export sentinel");
    assert!(
        export.status.success(),
        "task export from alpha must succeed: {}",
        String::from_utf8_lossy(&export.stderr)
    );
    let import = host
        .command(&alpha)
        .args([
            "--root",
            &root_str,
            "task",
            "import",
            &archive_str,
            "--task-workspace",
            "ws_unbound-data-dir",
        ])
        .output()
        .expect("import sentinel to the unbound partition");
    assert!(
        import.status.success(),
        "explicit-root task import must land in the unbound partition: {}",
        String::from_utf8_lossy(&import.stderr)
    );
    let unbound = host
        .command(&alpha)
        .args([
            "--root",
            &root_str,
            "tool",
            "run",
            "orbit.task.list",
            "--input",
            r#"{"model":"codex"}"#,
        ])
        .output()
        .expect("list unbound tasks");
    assert!(
        unbound.status.success(),
        "explicit-root task list must succeed: {}",
        String::from_utf8_lossy(&unbound.stderr)
    );
    let unbound_tasks: Value =
        serde_json::from_slice(&unbound.stdout).expect("unbound task list JSON");
    let unbound_list = unbound_tasks
        .get("tasks")
        .and_then(Value::as_array)
        .expect("task array");
    assert!(
        unbound_list
            .iter()
            .any(|task| task["title"].as_str() == Some(sentinel_title)),
        "fixture must seed the sentinel in the unbound partition: {unbound_tasks}"
    );

    let cwd = host.temp.path().join("unregistered");
    fs::create_dir(&cwd).expect("unregistered cwd");
    let identity = orbit_core::composition::compiled_compatibility();
    let digest = "a".repeat(64);
    let _record = GenerationGuard::acquire(&root, &digest).expect("record foreign digest");
    let _generation = GenerationGuard::join(
        &root,
        &Participant {
            digest: &digest,
            identity: &identity,
            role: ParticipantRole::Dashboard,
            access: Access::Write,
            handover: None,
            in_activity: false,
        },
        Duration::ZERO,
        || Ok(identity.store_schema.version),
    )
    .expect("hold live compatible foreign generation");
    let generation_before = fs::read(root.join(".generation.lock")).expect("generation record");
    let partitions = || {
        let mut entries = fs::read_dir(root.join("tasks/workspaces"))
            .expect("task partitions")
            .map(|entry| entry.expect("partition entry").file_name())
            .collect::<Vec<_>>();
        entries.sort();
        entries
    };
    let partitions_before = partitions();

    // A task-scoped read from an unregistered checkout must refuse with the
    // registration hint instead of opening the shared unbound partition that
    // holds the sentinel [ORB-19676].
    let output = host
        .command(&cwd)
        .args(["task", "list", "--json"])
        .output()
        .expect("run orbit task list");
    assert!(
        !output.status.success(),
        "unregistered task list must refuse without a cwd binding"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stderr}\n{stdout}");
    assert!(
        combined.contains("not inside a registered workspace")
            && combined.contains("orbit workspace init"),
        "unregistered task list must refuse with the registration hint: {combined}"
    );
    assert!(
        !combined.contains(sentinel_title),
        "unregistered task list must not return another data directory's tasks: {combined}"
    );
    assert!(
        !cwd.join(".orbit").exists(),
        "listing must not bind the cwd"
    );
    assert_eq!(
        partitions(),
        partitions_before,
        "listing must not mint a partition"
    );
    assert_eq!(
        fs::read(root.join(".generation.lock")).expect("generation record after listing"),
        generation_before,
        "listing must retain the live foreign generation"
    );
}

#[test]
fn dashboard_routine_status_registers_nothing_per_request() {
    let host = Host::new();
    let log = host.temp.path().join("dashboard.stderr");
    let mut server = BoundServer::spawn(
        host.command(host.temp.path())
            .args(["web", "serve", "--port", "0", "--no-open"])
            .stdout(Stdio::piped())
            .stderr(fs::File::create(&log).expect("dashboard log")),
        Announcement::Dashboard,
    )
    .expect("spawn orbit web serve");
    let address = server
        .wait_for_address(STARTUP_DEADLINE)
        .expect("dashboard bound");
    let at_start = registrations(&fs::read_to_string(&log).expect("read dashboard log"));

    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(STARTUP_DEADLINE)
        .build()
        .expect("HTTP client");
    let response = client
        .get(format!("http://{address}/api/routines"))
        .send()
        .expect("GET /api/routines");
    assert!(response.status().is_success(), "{}", response.status());
    let routines: Value = response.json().expect("routine status JSON");
    assert_eq!(
        sources(&routines),
        ["alpha", "beta", "gamma"],
        "routine status opens every registered checkout: {routines}"
    );

    let stderr = fs::read_to_string(&log).expect("read dashboard log");
    let _ = server.kill();
    let _ = server.wait();
    assert_eq!(
        at_start, 1,
        "the dashboard joins its generation once at start: {stderr}"
    );
    assert_eq!(
        registrations(&stderr),
        1,
        "a routine status request shares the dashboard's join: {stderr}"
    );
}
