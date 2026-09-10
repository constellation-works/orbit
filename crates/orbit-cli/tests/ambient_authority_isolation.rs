//! ORB-11300: an integration fixture must never inherit live workspace
//! authority.
//!
//! `ORBIT_REGISTRY_ROOT` selects the host-global registry and `ORBIT_WORKSPACE`
//! selects a workspace inside it, both outranking `HOME`. A suite launched from
//! inside a managed Orbit run inherits that pair, so fixtures that reset only
//! `HOME`/`ORBIT_ROOT` routed their `task add` writes into the *live*
//! workspace — three real task records were created that way before it was
//! caught.
//!
//! Every assertion here targets a disposable **sentinel**: a registry and
//! workspace built in a `TempDir` for this test alone. The reproducer names
//! that sentinel explicitly on the child command, so even the deliberately
//! unisolated case cannot reach production authority no matter what the
//! ambient environment holds.

#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;

use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

/// A disposable registry plus one registered workspace, standing in for the
/// live managed authority a leaking fixture would otherwise reach.
struct Sentinel {
    _temp: TempDir,
    home: PathBuf,
    work: PathBuf,
    workspace_id: String,
}

impl Sentinel {
    fn new() -> Self {
        let temp = tempdir().expect("sentinel tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        std::fs::create_dir_all(&home).expect("create sentinel home");
        std::fs::create_dir_all(&work).expect("create sentinel work");

        let mut command = isolated_orbit(&work, &home);
        run_ok(
            &mut command,
            &["workspace", "init", "--name", "orb-11300-sentinel"],
            "initialize sentinel workspace",
        );

        let registry: Value = serde_json::from_slice(
            &std::fs::read(home.join(".orbit/workspaces.json")).expect("read sentinel registry"),
        )
        .expect("parse sentinel registry");
        let workspace_id = registry["workspaces"][0]["id"]
            .as_str()
            .expect("sentinel workspace id")
            .to_string();

        Self {
            _temp: temp,
            home,
            work,
            workspace_id,
        }
    }

    fn registry_root(&self) -> PathBuf {
        self.home.join(".orbit")
    }

    fn task_dir(&self) -> PathBuf {
        self.registry_root()
            .join("tasks/workspaces")
            .join(&self.workspace_id)
    }

    /// Every byte the sentinel owns: its global registry and its workspace's
    /// task store.
    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        // Label each half: the registry and the task store both contain a
        // `.orbit/config.yaml`, and a colliding key would hide a change.
        collect_files(Path::new("registry"), &self.home, &self.home, &mut files);
        collect_files(Path::new("workspace"), &self.work, &self.work, &mut files);
        assert!(
            !files.is_empty(),
            "a sentinel with no files would make every assertion vacuous"
        );
        files
    }

    fn assert_unchanged(&self, before: &BTreeMap<PathBuf, Vec<u8>>) {
        let after = self.snapshot();
        let changed = before
            .iter()
            .filter(|(path, bytes)| after.get(*path).map(Vec::as_slice) != Some(bytes.as_slice()))
            .map(|(path, _)| path.display().to_string())
            .chain(
                after
                    .keys()
                    .filter(|path| !before.contains_key(*path))
                    .map(|path| format!("{} (created)", path.display())),
            )
            .collect::<Vec<_>>();
        assert!(
            changed.is_empty(),
            "fixture work reached the sentinel authority: {changed:?}"
        );
    }

    /// Present the sentinel to a child exactly the way a managed Orbit run
    /// presents the live authority to the suite it launches.
    fn export_authority(&self, command: &mut Command) {
        command
            .env("ORBIT_MANAGED_RUN_CONTEXT", "1")
            .env("ORBIT_RUN_ID", "jrun-orb-11300-regression")
            .env("ORBIT_REGISTRY_ROOT", self.registry_root())
            .env("ORBIT_WORKSPACE", &self.workspace_id);
    }
}

/// A temp workspace built the way every `orbit-cli` fixture builds one.
struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    work: PathBuf,
}

impl Fixture {
    /// Initialize under an ambient sentinel authority. The shared scrub inside
    /// [`isolated_orbit`] is the only thing keeping this off the sentinel.
    fn new(sentinel: &Sentinel) -> Self {
        let temp = tempdir().expect("fixture tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        std::fs::create_dir_all(&home).expect("create fixture home");
        std::fs::create_dir_all(&work).expect("create fixture work");

        let fixture = Self {
            _temp: temp,
            home,
            work,
        };
        let mut command = fixture.orbit(sentinel);
        run_ok(
            &mut command,
            &["workspace", "init", "--name", "orb-11300-fixture"],
            "initialize fixture workspace",
        );
        fixture
    }

    fn orbit(&self, sentinel: &Sentinel) -> Command {
        let mut command = cargo_bin_cmd!("orbit");
        sentinel.export_authority(&mut command);
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home);
        command
    }

    fn add_task(&self, sentinel: &Sentinel, title: &str) -> Value {
        let mut command = self.orbit(sentinel);
        let output = run_ok(
            &mut command,
            &[
                "task",
                "add",
                "--title",
                title,
                "--description",
                "ORB-11300 isolation regression.",
                "--complexity",
                "low",
                "--json",
            ],
            "add fixture task",
        );
        serde_json::from_slice(&output.stdout).expect("task add JSON")
    }

    fn task_dir(&self) -> PathBuf {
        let config: Value = serde_yaml::from_slice(
            &std::fs::read(self.work.join(".orbit/config.yaml"))
                .expect("read fixture workspace config"),
        )
        .expect("parse fixture workspace config");
        self.home.join(".orbit/tasks/workspaces").join(
            config["workspace_id"]
                .as_str()
                .expect("fixture workspace id"),
        )
    }
}

/// A leaking fixture is one that never clears the inherited pair. Point it at
/// the sentinel and it writes there — that is the defect this file guards, and
/// running it proves the sentinel is a real, reachable authority rather than an
/// inert directory that would make the isolated assertions vacuous.
#[test]
fn an_unscrubbed_child_routes_its_write_into_the_ambient_authority() {
    let sentinel = Sentinel::new();
    let before = sentinel.snapshot();

    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&work).expect("create work");

    // The pre-ORB-11300 fixture shape: pin `HOME`, drop `ORBIT_ROOT`, and
    // leave everything else inherited.
    let mut command = cargo_bin_cmd!("orbit");
    sentinel.export_authority(&mut command);
    let output = run_ok(
        command
            .current_dir(&work)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env_remove("ORBIT_ROOT"),
        &[
            "task",
            "add",
            "--title",
            "Unscrubbed probe",
            "--description",
            "ORB-11300 leak reproducer.",
            "--complexity",
            "low",
            "--json",
        ],
        "add task without the shared scrub",
    );

    let created: Value = serde_json::from_slice(&output.stdout).expect("task add JSON");
    let task_id = created["id"].as_str().expect("task id");
    assert!(
        sentinel.task_dir().join(task_id).exists(),
        "the reproducer must land in the sentinel, or the isolated cases below \
         prove nothing"
    );
    assert!(
        !home.join(".orbit").exists(),
        "resetting HOME did not redirect the write: {task_id}"
    );
    assert_ne!(
        sentinel.snapshot(),
        before,
        "the sentinel must be observably mutable"
    );
}

/// An explicit data root is a complete authority boundary even inside a
/// managed executor. The task-local `--workspace` remains task metadata; the
/// inherited managed selector must not be resolved in the scratch registry
/// before the command can use its explicitly selected root and cwd.
#[test]
fn explicit_root_allows_a_managed_child_to_round_trip_a_scratch_task_artifact() {
    let sentinel = Sentinel::new();
    let before = sentinel.snapshot();

    let scratch = tempdir().expect("scratch tempdir");
    let home = scratch.path().join("home");
    let root = scratch.path().join("root");
    let work = scratch.path().join("work");
    std::fs::create_dir_all(&home).expect("create scratch home");
    std::fs::create_dir_all(&work).expect("create scratch workspace");

    let root_arg = root.to_string_lossy().into_owned();
    let work_arg = work.to_string_lossy().into_owned();
    let mut command = managed_orbit(&work, &home, &sentinel);
    run_ok(
        &mut command,
        &[
            "--root",
            &root_arg,
            "init",
            "--non-interactive",
            "--host-name",
            "scratch-host",
            "--task-prefix",
            "SCR",
        ],
        "initialize scratch root",
    );

    let mut command = managed_orbit(&work, &home, &sentinel);
    run_ok(
        &mut command,
        &[
            "--root",
            &root_arg,
            "workspace",
            "init",
            "--name",
            "scratch-workspace",
            "--ship-mode",
            "local",
        ],
        "initialize scratch workspace",
    );

    let mut command = managed_orbit(&work, &home, &sentinel);
    let created = run_ok(
        &mut command,
        &[
            "--root",
            &root_arg,
            "task",
            "add",
            "--title",
            "Scratch artifact round trip",
            "--description",
            "Must stay inside the explicit scratch root.",
            "--complexity",
            "low",
            "--workspace",
            &work_arg,
            "--json",
        ],
        "add scratch task",
    );
    let created: Value = serde_json::from_slice(&created.stdout).expect("task add JSON");
    let task_id = created["id"].as_str().expect("task id");

    let source = work.join("summary.txt");
    let round_trip = work.join("round-trip.txt");
    std::fs::write(&source, "stored in scratch\n").expect("write source artifact");

    let source_arg = source.to_string_lossy().into_owned();
    let mut command = managed_orbit(&work, &home, &sentinel);
    run_ok(
        &mut command,
        &[
            "--root",
            &root_arg,
            "task",
            "artifact",
            "put",
            task_id,
            &source_arg,
            "--path",
            "reports/summary.txt",
            "--json",
        ],
        "store scratch task artifact",
    );

    let round_trip_arg = round_trip.to_string_lossy().into_owned();
    let mut command = managed_orbit(&work, &home, &sentinel);
    run_ok(
        &mut command,
        &[
            "--root",
            &root_arg,
            "task",
            "artifact",
            "get",
            task_id,
            "reports/summary.txt",
            "--out",
            &round_trip_arg,
            "--json",
        ],
        "read scratch task artifact",
    );

    assert_eq!(
        std::fs::read_to_string(&round_trip).expect("read round-trip artifact"),
        "stored in scratch\n"
    );
    assert!(
        root.join("tasks/workspaces/ws_scratch-workspace")
            .join(task_id)
            .is_dir(),
        "task must be stored in the scratch workspace partition"
    );
    sentinel.assert_unchanged(&before);
}

/// Initialization, creation, and mutation all route to the fixture's own
/// authority and leave the ambient sentinel byte-for-byte unchanged.
#[test]
fn fixture_lifecycle_leaves_the_ambient_authority_byte_for_byte_unchanged() {
    let sentinel = Sentinel::new();
    // Taken after `Sentinel::new`, so workspace initialization is the first
    // thing measured against it.
    let before = sentinel.snapshot();

    let fixture = Fixture::new(&sentinel);
    sentinel.assert_unchanged(&before);

    let created = fixture.add_task(&sentinel, "Isolated task");
    let task_id = created["id"].as_str().expect("task id").to_string();
    sentinel.assert_unchanged(&before);

    let mut command = fixture.orbit(&sentinel);
    let updated = run_ok(
        &mut command,
        &["task", "update", &task_id, "--tag", "docs", "--json"],
        "update fixture task",
    );
    let updated: Value = serde_json::from_slice(&updated.stdout).expect("task update JSON");
    assert_eq!(updated["tags"], serde_json::json!(["docs"]));
    sentinel.assert_unchanged(&before);

    assert!(
        fixture.task_dir().join(&task_id).exists(),
        "the write must land in the fixture's own workspace"
    );
    assert!(
        !fixture.work.join(".orbit/tasks").exists(),
        "fixture task mutations must not create checkout projections"
    );
}

/// The scrub is per-command and holds no shared state, so repeated fixtures
/// running concurrently under the same ambient authority must all stay off it.
#[test]
fn parallel_repeated_fixtures_all_stay_off_the_ambient_authority() {
    const THREADS: usize = 4;
    const ROUNDS: usize = 2;

    let sentinel = Arc::new(Sentinel::new());
    let before = sentinel.snapshot();

    let workers = (0..THREADS)
        .map(|thread| {
            let sentinel = Arc::clone(&sentinel);
            std::thread::spawn(move || {
                for round in 0..ROUNDS {
                    let fixture = Fixture::new(&sentinel);
                    let created =
                        fixture.add_task(&sentinel, &format!("Parallel {thread}-{round}"));
                    let task_id = created["id"].as_str().expect("task id");
                    assert!(
                        fixture.task_dir().join(task_id).exists(),
                        "thread {thread} round {round} wrote outside its own workspace"
                    );
                    assert!(!fixture.work.join(".orbit/tasks").exists());
                }
            })
        })
        .collect::<Vec<_>>();

    for worker in workers {
        worker.join().expect("fixture thread panicked");
    }

    sentinel.assert_unchanged(&before);
}

/// An `orbit` command with no inherited authority and no ambient sentinel —
/// used to build the sentinel itself.
fn isolated_orbit(cwd: &Path, home: &Path) -> Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home);
    command
}

/// A child with a complete, controlled managed-run authority envelope.
fn managed_orbit(cwd: &Path, home: &Path, sentinel: &Sentinel) -> Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    sentinel.export_authority(&mut command);
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home);
    command
}

fn run_ok(command: &mut Command, args: &[&str], label: &str) -> Output {
    let output = command.args(args).output().expect("run orbit");
    assert!(
        output.status.success(),
        "{label} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn collect_files(label: &Path, root: &Path, dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let entry = entry.expect("read sentinel entry");
        let path = entry.path();
        let kind = entry.file_type().expect("sentinel entry type");
        if kind.is_dir() {
            collect_files(label, root, &path, files);
        } else if kind.is_file() {
            let relative = label.join(path.strip_prefix(root).expect("relative path"));
            files.insert(relative, std::fs::read(&path).expect("read sentinel file"));
        }
    }
}
