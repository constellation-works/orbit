#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Binary-level coverage for generation admission isolation.
//!
//! `--root` and isolated `HOME` must be able to first-create `.generation.lock`
//! when the process home Orbit root is present, unpinned, and not writable —
//! those invocations only pin their own resolved root. `orbit update` is the
//! exception: it replaces the host binary, so it admits against the
//! host-global root as well and refuses when that record cannot be written.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::fs::generation::{
    GenerationGuard, ParticipantRecord, ParticipantRole, executable_generation,
};
use orbit_common::test_env;
use serde_json::Value;
use tempfile::tempdir;

const FOREIGN_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn explicit_root_spellings_select_the_same_generation_host_sweep_and_admission_roots() {
    for (raw, suffix) in [
        ("~", ""),
        ("~/x", "x"),
        ("~//x", "x"),
        ("~/.orbit", ".orbit"),
        ("./relative-root", "relative-root"),
    ] {
        let temp = tempfile::tempdir_in(test_env::canonical_temp_dir()).expect("fixture tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        fs::create_dir_all(&home).expect("home");
        crate::git_repo::init(&work);
        let root = if suffix.is_empty() {
            home.clone()
        } else if raw.starts_with('~') {
            home.join(suffix)
        } else {
            work.join(suffix)
        };
        initialize_registered_root(&work, &home, &root);

        for use_flag in [false, true] {
            let selected = || {
                let mut command = orbit(&work, &home);
                if use_flag {
                    command.args(["--root", raw]);
                    // A flag still outranks a different environment root.
                    command.env("ORBIT_ROOT", "unused-env-root");
                } else {
                    command.env("ORBIT_ROOT", raw);
                }
                command
            };
            selected()
                .args(["task", "list", "--json"])
                .assert()
                .success();
            let hosts = selected()
                .args(["host", "list", "--no-probe", "--json"])
                .assert()
                .success();
            let hosts: Value =
                serde_json::from_slice(&hosts.get_output().stdout).expect("hosts JSON");
            assert_eq!(hosts["hosts"][0]["name"], "root-spelling-host");

            let sweep = selected()
                .args(["run", "ship-sweep", "--dry-run", "--json"])
                .assert()
                .success();
            let sweep: Value =
                serde_json::from_slice(&sweep.get_output().stdout).expect("sweep JSON");
            assert_eq!(sweep["workspaces"], 1, "{raw}: {sweep}");
            assert_eq!(sweep["reports"][0]["workspace_name"], "root-spelling");

            let preflight = selected()
                .args(["update", "--preflight", "--json"])
                .output()
                .expect("preflight");
            let home_orbit = home.join(".orbit");
            let expected = if root == home_orbit {
                vec![root.as_path()]
            } else {
                vec![root.as_path(), home_orbit.as_path()]
            };
            assert_admitted_roots(&preflight, &expected);
            assert_generation_record(&root);
            assert!(
                !work.join("~").exists(),
                "{raw} created a literal tilde tree"
            );
            assert!(!work.join("unused-env-root").exists());
        }
    }
}

fn initialize_registered_root(work: &Path, home: &Path, root: &Path) {
    let root_arg = root.to_str().expect("UTF-8 root");
    orbit(work, home)
        .args([
            "--root",
            root_arg,
            "init",
            "--non-interactive",
            "--machine-name",
            "root-spelling-host",
            "--task-prefix",
            "RSP",
        ])
        .assert()
        .success();
    orbit(work, home)
        .args([
            "--root",
            root_arg,
            "workspace",
            "init",
            "--name",
            "root-spelling",
        ])
        .assert()
        .success();
}

#[test]
fn task_list_with_tilde_orbit_root_registers_its_participant_under_home() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let temp = tempfile::tempdir_in(test_env::canonical_temp_dir()).expect("fixture tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir_all(&home).expect("home");
    crate::git_repo::init(&work);
    let root = home.join(".orbit");
    initialize_registered_root(&work, &home, &root);

    // Hold the task index in rollback-journal mode so the built CLI cannot
    // finish its read before its live generation registration is inspected.
    let index = rusqlite::Connection::open(root.join("tasks/index.sqlite")).expect("task index");
    index
        .execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE;")
        .expect("hold task index read");
    let mut command = Command::new(env!("CARGO_BIN_EXE_orbit"));
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(&work)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("ORBIT_ROOT", "~/.orbit")
        .args(["task", "list", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("task list child");
    let deadline = Instant::now() + Duration::from_secs(10);
    let participant = loop {
        let found = fs::read_dir(root.join(".generation-participants"))
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|entry| fs::read(entry.path()).ok())
            .filter_map(|bytes| serde_json::from_slice::<ParticipantRecord>(&bytes).ok())
            .find(|record| record.pid == child.id());
        if found.is_some() || child.try_wait().expect("child status").is_some() {
            break found;
        }
        if Instant::now() >= deadline {
            child.kill().expect("stop unregistered child");
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    index
        .execute_batch("ROLLBACK;")
        .expect("release task index");
    let output = child.wait_with_output().expect("task list output");
    assert!(output.status.success(), "{output:?}");
    let participant = participant.expect("task list must register under the expanded HOME root");
    assert_eq!(participant.role, ParticipantRole::Command);
    assert!(!work.join("~").exists());
}

#[test]
fn clock_ticks_during_generation_hold_emit_one_dated_summary_on_resume() {
    let temp = tempdir().expect("fixture tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    let root = temp.path().join("orbit-root");
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&work).expect("work");
    let root_arg = root.to_str().expect("utf-8 root");
    let init = orbit(&work, &home)
        .args([
            "--root",
            root_arg,
            "init",
            "--non-interactive",
            "--machine-name",
            "qa-hold",
            "--task-prefix",
            "QXQA",
        ])
        .output()
        .expect("init root");
    assert!(init.status.success(), "{init:?}");

    let foreign = GenerationGuard::acquire(&root, FOREIGN_DIGEST).expect("foreign pin");
    for _ in 0..4 {
        let refused = orbit(&work, &home)
            .args(["--root", root_arg, "clock", "tick"])
            .output()
            .expect("refused clock tick");
        assert_eq!(refused.status.code(), Some(1), "{refused:?}");
        assert!(
            refused.stderr.is_empty(),
            "refused tick spammed log: {refused:?}"
        );
    }
    drop(foreign);
    let resumed = orbit(&work, &home)
        .args(["--root", root_arg, "clock", "tick"])
        .output()
        .expect("resumed clock tick");
    assert!(resumed.status.success(), "{resumed:?}");
    let log = String::from_utf8_lossy(&resumed.stderr);
    assert_eq!(log.lines().count(), 1, "{log}");
    assert!(
        log.contains("started_at=") && log.contains("ended_at=") && log.contains("refused_ticks=4"),
        "{log}"
    );
}

fn orbit(work: &Path, home: &Path) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home);
    command
}

#[cfg(unix)]
struct ReadOnlyDir {
    path: PathBuf,
}

#[cfg(unix)]
impl ReadOnlyDir {
    fn enter(path: PathBuf) -> Self {
        fs::create_dir_all(&path).expect("create directory to freeze");
        chmod(&path, 0o555);
        Self { path }
    }
}

#[cfg(unix)]
impl Drop for ReadOnlyDir {
    fn drop(&mut self) {
        chmod(&self.path, 0o755);
    }
}

#[cfg(unix)]
fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)
        .unwrap_or_else(|error| panic!("metadata {}: {error}", path.display()))
        .permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)
        .unwrap_or_else(|error| panic!("chmod {}: {error}", path.display()));
}

fn assert_generation_record(root: &Path) {
    let record = fs::read_to_string(root.join(".generation.lock")).expect("generation lock");
    assert!(
        record.starts_with("1:") && record.trim_end().len() == 66,
        "expected a v1 digest record, got {record:?}"
    );
}

#[cfg(unix)]
#[test]
fn init_with_root_succeeds_when_home_orbit_is_readonly_and_unpinned() {
    let temp = tempdir().expect("fixture tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    let scratch = temp.path().join("scratch");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&work).expect("create work");
    let home_orbit = home.join(".orbit");
    let _frozen = ReadOnlyDir::enter(home_orbit.clone());

    let output = orbit(&work, &home)
        .args([
            "--root",
            scratch.to_str().expect("utf-8 scratch"),
            "init",
            "--non-interactive",
            "--machine-name",
            "qa-root",
            "--task-prefix",
            "QXQA",
        ])
        .output()
        .expect("run --root init");
    assert!(
        output.status.success(),
        "--root init failed against a read-only home Orbit root\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_generation_record(&scratch);
    assert!(scratch.join("config.toml").is_file());
    assert!(!home_orbit.join(".generation.lock").exists());
    assert!(!home_orbit.join(".generation-admission.lock").exists());
}

#[test]
fn init_with_isolated_home_writes_generation_lock_under_home_orbit() {
    let temp = tempdir().expect("fixture tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&work).expect("create work");

    let output = orbit(&work, &home)
        .args([
            "init",
            "--non-interactive",
            "--machine-name",
            "qa-home",
            "--task-prefix",
            "QXQA",
        ])
        .output()
        .expect("run isolated-home init");
    assert!(
        output.status.success(),
        "isolated HOME init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let home_orbit = home.join(".orbit");
    assert_generation_record(&home_orbit);
    assert!(home_orbit.join("config.toml").is_file());
}

/// `orbit update` replaces the host binary whatever `--root` says, so the
/// host-global authority has to be able to record the candidate. A `~/.orbit`
/// this process cannot write is refused by the probe, not discovered after
/// the executable has already been swapped.
#[cfg(unix)]
#[test]
fn preflight_with_root_refuses_an_unwritable_host_global_authority() {
    let temp = tempdir().expect("fixture tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    let scratch = temp.path().join("scratch");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&work).expect("create work");
    let home_orbit = home.join(".orbit");
    let _frozen = ReadOnlyDir::enter(home_orbit.clone());

    let output = orbit(&work, &home)
        .args([
            "--root",
            scratch.to_str().expect("utf-8 scratch"),
            "update",
            "--preflight",
            "--json",
        ])
        .output()
        .expect("run --root preflight");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("upgrade admission refused"), "{stderr}");
    assert!(
        stderr.contains(&home_orbit.display().to_string()),
        "the refusal must name the authority it could not record against: {stderr}"
    );
    assert!(!home_orbit.join(".generation.lock").exists());
    assert!(!home_orbit.join(".generation-admission.lock").exists());
}

/// A `--root` override stays isolated for state, and admission over a
/// writable host-global root still reports both authorities.
#[cfg(unix)]
#[test]
fn preflight_with_root_reports_both_authorities_when_home_orbit_is_writable() {
    let temp = tempdir().expect("fixture tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    let scratch = temp.path().join("scratch");
    fs::create_dir_all(&home).expect("create home");
    crate::git_repo::init(&work);
    // Stop walk-up without adding an initialized cwd workspace authority.
    fs::create_dir_all(work.join(".orbit")).expect("uninitialized lookup boundary");
    let home_orbit = home.join(".orbit");
    fs::create_dir_all(&home_orbit).expect("create home orbit");

    let output = orbit(&work, &home)
        .args([
            "--root",
            scratch.to_str().expect("utf-8 scratch"),
            "update",
            "--preflight",
            "--json",
        ])
        .output()
        .expect("run --root preflight");
    assert!(
        output.status.success(),
        "--root preflight failed against a writable home Orbit root\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("preflight JSON");
    assert_eq!(report["admitted"], true);
    assert_eq!(report["reservation"], false);
    assert_eq!(report["contract"], "executable-generation-v1");
    assert_eq!(report["global_root"], scratch.to_string_lossy().as_ref());
    assert_eq!(
        report["admission_roots"][1],
        home_orbit.to_string_lossy().as_ref()
    );
    assert!(
        scratch.join(".generation.lock").is_file()
            || scratch.join(".generation-admission.lock").is_file(),
        "preflight should create coordination lock files under --root"
    );
}

#[test]
fn preflight_on_isolated_home_is_blocked_by_a_live_exclusive_holder() {
    let temp = tempdir().expect("fixture tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&work).expect("create work");
    let home_orbit = home.join(".orbit");
    fs::create_dir_all(&home_orbit).expect("create home orbit");
    let digest = executable_generation(Path::new(env!("CARGO_BIN_EXE_orbit"))).expect("digest");
    let _holder = GenerationGuard::acquire(&home_orbit, &digest).expect("live same-digest pin");

    let output = orbit(&work, &home)
        .args(["update", "--preflight", "--json"])
        .output()
        .expect("run preflight against live holder");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Orbit clients or commands are still running"),
        "{stderr}"
    );
}

#[test]
fn isolated_home_refuses_a_different_executable_digest_while_a_pin_is_held() {
    let temp = tempdir().expect("fixture tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&work).expect("create work");
    let home_orbit = home.join(".orbit");
    fs::create_dir_all(&home_orbit).expect("create home orbit");
    let _pin = GenerationGuard::acquire(&home_orbit, FOREIGN_DIGEST).expect("foreign pin");

    let output = orbit(&work, &home)
        .args([
            "init",
            "--non-interactive",
            "--machine-name",
            "qa-digest",
            "--task-prefix",
            "QXQA",
        ])
        .output()
        .expect("run init against foreign pin");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("another executable generation is still running"),
        "{stderr}"
    );
}

#[test]
fn init_with_root_ignores_a_foreign_pin_on_home_orbit() {
    let temp = tempdir().expect("fixture tempdir");
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    let scratch = temp.path().join("scratch");
    fs::create_dir_all(&home).expect("create home");
    fs::create_dir_all(&work).expect("create work");
    let home_orbit = home.join(".orbit");
    fs::create_dir_all(&home_orbit).expect("create home orbit");
    let _pin = GenerationGuard::acquire(&home_orbit, FOREIGN_DIGEST).expect("foreign pin");

    let output = orbit(&work, &home)
        .args([
            "--root",
            scratch.to_str().expect("utf-8 scratch"),
            "init",
            "--non-interactive",
            "--machine-name",
            "qa-isolated",
            "--task-prefix",
            "QXQA",
        ])
        .output()
        .expect("run --root init against foreign home pin");
    assert!(
        output.status.success(),
        "--root init should isolate from a home pin\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_generation_record(&scratch);
    let home_record = fs::read_to_string(home_orbit.join(".generation.lock")).expect("home pin");
    assert_eq!(home_record, format!("1:{FOREIGN_DIGEST}\n"));
}

struct PreflightTrees {
    _temp: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
    scratch: PathBuf,
    home_orbit: PathBuf,
    workspace_orbit: PathBuf,
    /// Workspace path the child reports. Process cwd is physical, so a fixture
    /// created through macOS `/var` (a symlink to `/private/var`) is discovered
    /// as `/private/var/.../work/.orbit`. Override and home paths stay as given.
    discovered_workspace_orbit: PathBuf,
}

impl PreflightTrees {
    fn new(initialize_workspace: bool) -> Self {
        let temp = tempdir().expect("fixture tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        let scratch = temp.path().join("scratch");
        fs::create_dir_all(&home).expect("create home");
        crate::git_repo::init(&work);
        fs::create_dir_all(&scratch).expect("create scratch");
        let home_orbit = home.join(".orbit");
        fs::create_dir_all(&home_orbit).expect("create home orbit");
        let workspace_orbit = work.join(".orbit");
        if initialize_workspace {
            fs::create_dir_all(&workspace_orbit).expect("create workspace orbit");
            // No `root` key: discovery must keep this directory rather than
            // redirecting at a configured path.
            fs::write(workspace_orbit.join("config.toml"), "# fixture workspace\n")
                .expect("workspace config");
        }
        let discovered_workspace_orbit = fs::canonicalize(&work)
            .unwrap_or_else(|error| panic!("canonicalize work {}: {error}", work.display()))
            .join(".orbit");
        Self {
            _temp: temp,
            home,
            work,
            scratch,
            home_orbit,
            workspace_orbit,
            discovered_workspace_orbit,
        }
    }
}

fn preflight_output(
    trees: &PreflightTrees,
    root_flag: Option<&Path>,
    orbit_root: Option<&Path>,
) -> std::process::Output {
    let mut command = orbit(&trees.work, &trees.home);
    if let Some(root) = orbit_root {
        command.env("ORBIT_ROOT", root);
    } else {
        command.env_remove("ORBIT_ROOT");
    }
    let mut args = Vec::new();
    if let Some(root) = root_flag {
        args.push("--root".to_string());
        args.push(root.to_string_lossy().into_owned());
    }
    args.extend([
        "update".to_string(),
        "--preflight".to_string(),
        "--json".to_string(),
    ]);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    command.args(arg_refs).output().expect("run preflight")
}

fn assert_admitted_roots(output: &std::process::Output, expected: &[&Path]) {
    assert!(
        output.status.success(),
        "preflight failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("preflight JSON");
    assert_eq!(report["admitted"], true);
    assert_eq!(report["reservation"], false);
    assert_eq!(report["contract"], "executable-generation-v1");
    let roots: Vec<Value> = expected
        .iter()
        .map(|path| Value::String(path.to_string_lossy().into_owned()))
        .collect();
    assert_eq!(report["admission_roots"], Value::Array(roots.clone()));
    assert_eq!(report["global_root"], roots[0]);
}

/// An uninitialized `--root` is still a generation authority. Preflight also
/// admits the host-global root and the initialized workspace discovered from
/// the working directory, and reports that nothing was reserved.
#[test]
fn preflight_with_root_admits_uninitialized_override_cwd_and_host_global() {
    let trees = PreflightTrees::new(true);
    let output = preflight_output(&trees, Some(&trees.scratch), None);
    assert_admitted_roots(
        &output,
        &[
            trees.scratch.as_path(),
            trees.home_orbit.as_path(),
            trees.discovered_workspace_orbit.as_path(),
        ],
    );
    assert!(
        !trees.scratch.join("config.toml").exists(),
        "preflight must not initialize the explicit generation root"
    );
}

/// `ORBIT_ROOT` is the same explicit generation root as `--root`.
#[test]
fn preflight_with_orbit_root_admits_uninitialized_override_cwd_and_host_global() {
    let trees = PreflightTrees::new(true);
    let output = preflight_output(&trees, None, Some(&trees.scratch));
    assert_admitted_roots(
        &output,
        &[
            trees.scratch.as_path(),
            trees.home_orbit.as_path(),
            trees.discovered_workspace_orbit.as_path(),
        ],
    );
}

/// A live client in the discovered workspace refuses the probe for both
/// spellings of an uninitialized override. The override does not drop that
/// authority.
#[test]
fn preflight_refuses_a_live_cwd_client_for_root_and_orbit_root() {
    let trees = PreflightTrees::new(true);
    let digest = executable_generation(Path::new(env!("CARGO_BIN_EXE_orbit"))).expect("digest");
    let _holder =
        GenerationGuard::acquire(&trees.workspace_orbit, &digest).expect("live workspace pin");

    for output in [
        preflight_output(&trees, Some(&trees.scratch), None),
        preflight_output(&trees, None, Some(&trees.scratch)),
    ] {
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("Orbit clients or commands are still running"),
            "{stderr}"
        );
        assert!(
            stderr.contains(&trees.discovered_workspace_orbit.display().to_string()),
            "the refusal must name the cwd workspace authority: {stderr}"
        );
    }
}

/// An initialized `--root` selects that workspace for convergence. A different
/// cwd workspace is not an extra authority, matching the updater.
#[test]
fn preflight_with_initialized_root_does_not_add_a_different_cwd_workspace() {
    let trees = PreflightTrees::new(true);
    let other = trees._temp.path().join("other");
    let other_orbit = other.join(".orbit");
    fs::create_dir_all(&other_orbit).expect("other workspace");
    fs::write(other_orbit.join("config.toml"), "# other workspace\n").expect("other config");

    let output = preflight_output(&trees, Some(&other_orbit), None);
    assert_admitted_roots(
        &output,
        &[other_orbit.as_path(), trees.home_orbit.as_path()],
    );
}

/// Two spellings of one authority stay one entry: a symlink to the host-global
/// root plus that root itself. The distinct cwd workspace is still admitted.
#[cfg(unix)]
#[test]
fn preflight_deduplicates_a_host_global_alias_and_keeps_the_cwd_workspace() {
    let trees = PreflightTrees::new(true);
    let alias = trees._temp.path().join("host-alias");
    std::os::unix::fs::symlink(&trees.home_orbit, &alias).expect("symlink host-global root");

    let output = preflight_output(&trees, Some(&alias), None);
    assert_admitted_roots(
        &output,
        &[alias.as_path(), trees.discovered_workspace_orbit.as_path()],
    );
}

/// A workspace config that cannot be read is not turned into "no workspace".
#[test]
fn preflight_reports_a_broken_cwd_workspace_config() {
    let trees = PreflightTrees::new(true);
    fs::write(trees.workspace_orbit.join("config.toml"), "root = [\n").expect("break config");

    let output = preflight_output(&trees, Some(&trees.scratch), None);
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid runtime config"),
        "unrelated resolver errors must surface: {stderr}"
    );
    assert!(
        !stderr.contains("is not an Orbit workspace"),
        "the uninitialized generation root must not replace the config error: {stderr}"
    );
}

/// `orbit update` still refuses an explicit root that is not a workspace.
/// Preflight's probe does not relax convergence.
#[test]
fn update_with_uninitialized_root_still_requires_a_workspace() {
    let trees = PreflightTrees::new(false);
    for (label, mut command) in [
        ("--root", {
            let mut command = orbit(&trees.work, &trees.home);
            command.args([
                "--root",
                trees.scratch.to_str().expect("utf-8 scratch"),
                "update",
                "--check",
                "--json",
            ]);
            command
        }),
        ("ORBIT_ROOT", {
            let mut command = orbit(&trees.work, &trees.home);
            command
                .env("ORBIT_ROOT", &trees.scratch)
                .args(["update", "--check", "--json"]);
            command
        }),
    ] {
        let output = command.output().expect("run update --check");
        assert!(
            !output.status.success(),
            "{label} update should refuse an uninitialized root\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("is not an Orbit workspace"),
            "{label} stderr: {stderr}"
        );
        assert!(
            !trees.scratch.join("config.toml").exists(),
            "{label} must not initialize the explicit root"
        );
    }
}
