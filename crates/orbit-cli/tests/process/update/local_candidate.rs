//! `orbit update --local-candidate` through the public binary.
//!
//! The installation is a managed copy of this test's `orbit` in an isolated
//! `HOME`, beside an initialized workspace. A candidate "built at commit B" is
//! a distinct copy of the same binary: equal semantic version, different
//! bytes — exactly the shift-build case a release version cannot tell apart.
//! Every update runs through the candidate's own updater with an explicit
//! `--install-target`, the bootstrap an older installed binary needs.

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use orbit_common::fs::generation::{GenerationGuard, executable_generation};
use orbit_common::test_env;
use serde_json::Value;
use tempfile::TempDir;

use super::integrity::{
    ReapedChild, backup_path, mkfifo, open_fifo_writer, staging_remains, wait_exit,
};

const COMMIT_B: &str = "b1b2b3b4b5b6b7b8b9b0b1b2b3b4b5b6b7b8b9b0";
const OTHER_COMMIT: &str = "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00";
const RENDEZVOUS_DEADLINE: Duration = Duration::from_secs(60);

struct LocalInstall {
    root: TempDir,
    home: PathBuf,
    repo: PathBuf,
    bin: PathBuf,
    installed: PathBuf,
    builds: PathBuf,
}

impl LocalInstall {
    /// An initialized workspace and a managed installation of this build.
    fn new() -> Self {
        let root = tempfile::tempdir().expect("fixture root");
        let home = root.path().join("home");
        let repo = root.path().join("repo");
        let bin = root.path().join("managed-bin");
        let builds = root.path().join("builds");
        let mirror = root.path().join("mirror");
        for directory in [&home, &builds, &mirror] {
            fs::create_dir_all(directory).expect("fixture directory");
        }
        super::init_git_repo(&repo);
        super::orbit(&repo, &home, &mirror)
            .args([
                "init",
                "--non-interactive",
                "--skip-host-prerequisites",
                "--machine-name",
                "local-candidate",
                "--task-prefix",
                "LC",
            ])
            .assert()
            .success();
        super::orbit(&repo, &home, &mirror)
            .args(["workspace", "init", "--name", "local-candidate"])
            .assert()
            .success();
        let installed = super::install_test_binary(&bin);
        Self {
            root,
            home,
            repo,
            bin,
            installed,
            builds,
        }
    }

    /// `program` in the fixture's isolated environment, from the workspace.
    fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("ORBIT_INSTALL_DIR", &self.bin)
            .env_remove("ORBIT_HOME")
            .env_remove("ORBIT_ROOT")
            .env_remove("ORBIT_REGISTRY_ROOT")
            .env_remove("ORBIT_WORKSPACE")
            .env_remove("ORBIT_UPDATE_RELEASE_DIR")
            .stdin(Stdio::null());
        command
    }

    fn run(&self, program: &Path, args: &[OsString]) -> Output {
        let mut command = self.command(program);
        command.args(args);
        retry_busy(&mut command)
    }

    /// Run a reported recovery command without the install-dir override used
    /// by `command`, as when an operator copies it into a later shell.
    fn retry_command(&self, retry: &str) -> Output {
        let mut command = Command::new("sh");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env_remove("ORBIT_INSTALL_DIR")
            .env_remove("ORBIT_HOME")
            .env_remove("ORBIT_ROOT")
            .env_remove("ORBIT_REGISTRY_ROOT")
            .env_remove("ORBIT_WORKSPACE")
            .env_remove("ORBIT_UPDATE_RELEASE_DIR")
            .stdin(Stdio::null())
            .arg("-c")
            .arg(retry);
        command.output().expect("run reported recovery command")
    }

    /// A candidate built at another commit: same version, distinct bytes.
    fn candidate(&self, name: &str) -> PathBuf {
        let candidate = self.builds.join(name);
        crate::generation_fixture::distinct_copy(
            Path::new(env!("CARGO_BIN_EXE_orbit")),
            &candidate,
        );
        candidate
    }

    /// Describe `candidate` with the public producer, run by the candidate.
    fn manifest(&self, candidate: &Path, commit: &str, name: &str) -> PathBuf {
        self.manifest_by(candidate, candidate, commit, name)
    }

    /// Describe `candidate` with `producer`'s public producer, for candidates
    /// that cannot run here.
    fn manifest_by(&self, producer: &Path, candidate: &Path, commit: &str, name: &str) -> PathBuf {
        let manifest = self.builds.join(name);
        let output = self.run(
            producer,
            &args(&[
                "update".as_ref(),
                "--local-candidate".as_ref(),
                candidate.as_os_str(),
                "--source-commit".as_ref(),
                commit.as_ref(),
                "--write-candidate-manifest".as_ref(),
                manifest.as_os_str(),
                "--json".as_ref(),
            ]),
        );
        assert_success(&output, "write the candidate manifest");
        manifest
    }

    fn install_args(&self, candidate: &Path, manifest: &Path, commit: &str) -> Vec<OsString> {
        args(&[
            "update".as_ref(),
            "--local-candidate".as_ref(),
            candidate.as_os_str(),
            "--candidate-manifest".as_ref(),
            manifest.as_os_str(),
            "--source-commit".as_ref(),
            commit.as_ref(),
            "--install-target".as_ref(),
            self.installed.as_os_str(),
            "--json".as_ref(),
        ])
    }

    fn generation_record(&self) -> Option<Vec<u8>> {
        fs::read(self.home.join(".orbit/.generation.lock")).ok()
    }

    /// The refusal changed neither the installation nor the generation record.
    fn assert_refused(
        &self,
        output: &Output,
        needle: &str,
        before: &[u8],
        record: &Option<Vec<u8>>,
    ) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(1),
            "expected refusal containing {needle:?}\nstdout:\n{}\nstderr:\n{stderr}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(stderr.contains(needle), "stderr lacks {needle:?}: {stderr}");
        assert_eq!(
            fs::read(&self.installed).expect("installed bytes"),
            before,
            "{needle}: the installed executable changed"
        );
        assert!(
            !backup_path(&self.installed).exists(),
            "{needle}: wrote a backup"
        );
        assert!(!staging_remains(&self.bin), "{needle}: left a staging file");
        assert_eq!(
            &self.generation_record(),
            record,
            "{needle}: rewrote the generation record"
        );
    }
}

fn args(values: &[&std::ffi::OsStr]) -> Vec<OsString> {
    values.iter().map(|value| value.to_os_string()).collect()
}

/// Run a freshly copied binary, absorbing the parallel-fork `ETXTBSY` race.
fn retry_busy(command: &mut Command) -> Output {
    #[cfg(target_os = "linux")]
    {
        orbit_common::test_process::retry_executable_busy(|| command.output()).expect("run orbit")
    }
    #[cfg(not(target_os = "linux"))]
    {
        command.output().expect("run orbit")
    }
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed ({:?})\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "update report is not JSON ({error})\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn sha256(path: &Path) -> String {
    executable_generation(path).expect("digest")
}

fn assert_converged(report: &Value) {
    let steps = report["steps"].as_array().expect("steps");
    let commands: Vec<&str> = steps
        .iter()
        .map(|step| step["command"].as_str().expect("command"))
        .collect();
    assert_eq!(
        commands,
        ["migrate --confirm", "workspace sync", "clock repair"],
        "{report}"
    );
    assert!(
        steps.iter().all(|step| step["status"] == "succeeded"),
        "{report}"
    );
}

/// The installed binary predates `--local-candidate` (a stand-in that knows
/// only `--version`, as an older release's argument parser would refuse the
/// option). The newly built candidate bootstraps the capability by running
/// the update itself, against the explicitly named installed target.
#[test]
fn an_older_installation_is_bootstrapped_to_an_equal_version_candidate_and_replays_idempotently() {
    let install = LocalInstall::new();
    let version = env!("CARGO_PKG_VERSION");
    fs::write(
        &install.installed,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = --version ]; then echo 'orbit {version}'; exit 0; fi\n\
             echo \"error: unexpected argument '$2' found\" >&2\nexit 2\n"
        ),
    )
    .expect("install the older binary");
    let older = fs::read(&install.installed).expect("older bytes");
    let candidate = install.candidate("orbit-b");
    let manifest = install.manifest(&candidate, COMMIT_B, "orbit-b.json");

    // The installed updater cannot do this itself.
    let refused = install.run(
        &install.installed,
        &install.install_args(&candidate, &manifest, COMMIT_B),
    );
    assert_eq!(refused.status.code(), Some(2), "{refused:?}");
    assert_eq!(fs::read(&install.installed).expect("installed"), older);

    let attested: Value =
        serde_json::from_slice(&fs::read(&manifest).expect("manifest")).expect("manifest JSON");
    assert_eq!(attested["kind"], "orbit-local-candidate");
    assert_eq!(attested["trust"], "operator_attested");
    assert_eq!(attested["source_commit"], COMMIT_B);
    assert_eq!(attested["executable_sha256"], sha256(&candidate));

    let output = install.run(
        &candidate,
        &install.install_args(&candidate, &manifest, COMMIT_B),
    );
    assert_success(&output, "bootstrap the local candidate");
    let updated = report(&output);
    assert_eq!(updated["outcome"], "updated", "{updated}");
    assert_eq!(updated["replaced"], true);
    assert_eq!(updated["current_version"], version);
    assert_eq!(
        updated["target_version"], version,
        "equal versions still replace"
    );
    assert!(updated.get("signing_key_id").is_none(), "{updated}");
    assert!(updated.get("archive_sha256").is_none(), "{updated}");
    let local = &updated["local_candidate"];
    assert_eq!(local["trust"], "operator_attested");
    assert_eq!(local["signed_release"], false);
    assert_eq!(local["source_commit"]["value"], COMMIT_B);
    assert_eq!(local["source_commit"]["evidence"], "operator_attested");
    assert_eq!(local["executable_sha256"]["value"], sha256(&candidate));
    assert_eq!(
        local["installed_sha256_before"],
        sha256(&backup_path(&install.installed))
    );
    assert_eq!(local["installed_sha256_after"], sha256(&candidate));
    let admission_roots = updated["admission_roots"]
        .as_array()
        .expect("admission roots");
    let host_global_root = install.home.join(".orbit");
    let workspace_root = updated["workspace_root"]
        .as_str()
        .expect("selected workspace root");
    assert!(
        admission_roots
            .iter()
            .any(|root| root.as_str() == host_global_root.to_str()),
        "host-global authority remains admitted: {updated}"
    );
    assert!(
        admission_roots
            .iter()
            .any(|root| root.as_str() == Some(workspace_root)),
        "the convergence workspace is admitted: {updated}"
    );
    assert_converged(&updated);
    assert_eq!(
        fs::read(&install.installed).expect("installed"),
        fs::read(&candidate).expect("candidate")
    );
    assert_eq!(
        fs::read(backup_path(&install.installed)).expect("backup"),
        older
    );

    // Replay through the now-installed updater: nothing is swapped again, and
    // convergence runs to completion.
    let replay = install.run(
        &install.installed,
        &install.install_args(&candidate, &manifest, COMMIT_B),
    );
    assert_success(&replay, "replay the accepted candidate");
    let replayed = report(&replay);
    assert_eq!(replayed["outcome"], "already_current", "{replayed}");
    assert_eq!(replayed["replaced"], false);
    assert_converged(&replayed);
    assert_eq!(
        fs::read(backup_path(&install.installed)).expect("backup"),
        older,
        "a replay must not overwrite the preserved previous executable"
    );
}

#[test]
fn wrong_provenance_digest_target_or_ownership_refuses_before_replacement() {
    let install = LocalInstall::new();
    let before = fs::read(&install.installed).expect("installed bytes");
    let record = install.generation_record();
    let candidate = install.candidate("orbit-b");
    let manifest = install.manifest(&candidate, COMMIT_B, "orbit-b.json");
    let run = |program: &Path, args: Vec<OsString>| install.run(program, &args);

    // Provenance: the operator expects another commit, or abbreviates it.
    let output = run(
        &candidate,
        install.install_args(&candidate, &manifest, OTHER_COMMIT),
    );
    install.assert_refused(&output, "attests source commit", &before, &record);
    let output = run(
        &candidate,
        install.install_args(&candidate, &manifest, &COMMIT_B[..12]),
    );
    install.assert_refused(&output, "full Git commit", &before, &record);

    // Manifest: missing, malformed, or another schema.
    let missing = install.builds.join("missing.json");
    let output = run(
        &candidate,
        install.install_args(&candidate, &missing, COMMIT_B),
    );
    install.assert_refused(
        &output,
        "cannot read the local-candidate manifest",
        &before,
        &record,
    );
    let malformed = install.builds.join("malformed.json");
    fs::write(&malformed, "{\"schema_version\":1").expect("malformed manifest");
    let output = run(
        &candidate,
        install.install_args(&candidate, &malformed, COMMIT_B),
    );
    install.assert_refused(
        &output,
        "not a valid orbit-local-candidate",
        &before,
        &record,
    );
    let mut future: Value =
        serde_json::from_slice(&fs::read(&manifest).expect("manifest")).expect("JSON");
    future["schema_version"] = 2.into();
    let unsupported = install.builds.join("v2.json");
    fs::write(&unsupported, future.to_string()).expect("v2 manifest");
    let output = run(
        &candidate,
        install.install_args(&candidate, &unsupported, COMMIT_B),
    );
    install.assert_refused(
        &output,
        "unsupported local-candidate manifest",
        &before,
        &record,
    );

    // Digest: the candidate changed after its manifest was written.
    let changed = install.builds.join("orbit-changed");
    fs::copy(&candidate, &changed).expect("copy candidate");
    fs::OpenOptions::new()
        .append(true)
        .open(&changed)
        .and_then(|mut file| file.write_all(b"rebuilt"))
        .expect("change candidate");
    let output = run(
        &candidate,
        install.install_args(&changed, &manifest, COMMIT_B),
    );
    install.assert_refused(
        &output,
        "changed after its manifest was written",
        &before,
        &record,
    );

    // Target: an executable built for another architecture.
    let foreign = install.builds.join("orbit-foreign");
    fs::copy(&candidate, &foreign).expect("copy candidate");
    retarget(&foreign);
    let foreign_manifest = install.manifest_by(&candidate, &foreign, COMMIT_B, "foreign.json");
    let output = run(
        &candidate,
        install.install_args(&foreign, &foreign_manifest, COMMIT_B),
    );
    install.assert_refused(&output, "but this installation needs", &before, &record);

    // Ownership: outside the managed directory, a symlink, or another name.
    let elsewhere = install.root.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).expect("unmanaged directory");
    fs::copy(&install.installed, elsewhere.join("orbit")).expect("unmanaged copy");
    let link = install.bin.join("orbit-link");
    std::os::unix::fs::symlink(&install.installed, &link).expect("symlink");
    for (target, needle) in [
        (elsewhere.join("orbit"), "unknown owns it"),
        (link.clone(), "the managed `orbit` executable"),
    ] {
        let mut args = install.install_args(&candidate, &manifest, COMMIT_B);
        let position = args
            .iter()
            .position(|arg| arg == "--install-target")
            .expect("flag");
        args[position + 1] = target.into_os_string();
        let output = run(&candidate, args);
        install.assert_refused(&output, needle, &before, &record);
    }
    fs::remove_file(&install.installed).expect("unlink installed");
    std::os::unix::fs::symlink(elsewhere.join("orbit"), &install.installed).expect("symlink orbit");
    let output = run(
        &candidate,
        install.install_args(&candidate, &manifest, COMMIT_B),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("symbolic link"), "{stderr}");
    assert_eq!(
        fs::read_link(&install.installed).expect("still a link"),
        elsewhere.join("orbit")
    );
    fs::remove_file(&install.installed).expect("remove link");
    fs::write(&install.installed, &before).expect("restore installed");
    fs::set_permissions(
        &install.installed,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("chmod installed");

    // The install target is never inferred, and a release cannot be mixed in.
    let output = run(
        &candidate,
        args(&[
            "update".as_ref(),
            "--local-candidate".as_ref(),
            candidate.as_os_str(),
            "--source-commit".as_ref(),
            COMMIT_B.as_ref(),
            "--candidate-manifest".as_ref(),
            manifest.as_os_str(),
        ]),
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("--install-target"));
    let mut mixed = install.install_args(&candidate, &manifest, COMMIT_B);
    mixed.extend(args(&["--version".as_ref(), "99.0.0".as_ref()]));
    assert_eq!(run(&candidate, mixed).status.code(), Some(2));
    let mut mixed = install.install_args(&candidate, &manifest, COMMIT_B);
    mixed.push("--contract".into());
    assert_eq!(
        run(&candidate, mixed).status.code(),
        Some(2),
        "--contract must not silently bypass a local-candidate update"
    );

    // The producer never overwrites a file, including the candidate itself.
    let output = run(
        &candidate,
        args(&[
            "update".as_ref(),
            "--local-candidate".as_ref(),
            candidate.as_os_str(),
            "--source-commit".as_ref(),
            COMMIT_B.as_ref(),
            "--write-candidate-manifest".as_ref(),
            candidate.as_os_str(),
        ]),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("never overwritten"));
    assert_eq!(sha256(&candidate), sha256(&install.builds.join("orbit-b")));

    // A live host-global client refuses an update whose --root names the
    // workspace instead: the override does not move the replaced executable.
    let _client =
        GenerationGuard::acquire(&install.home.join(".orbit"), &sha256(&install.installed))
            .expect("live host-global pin");
    let record = install.generation_record();
    let mut overridden = args(&["--root".as_ref(), install.repo.join(".orbit").as_os_str()]);
    overridden.extend(install.install_args(&candidate, &manifest, COMMIT_B));
    let output = run(&candidate, overridden);
    install.assert_refused(&output, "upgrade admission refused", &before, &record);
}

/// A discovered workspace can differ from the isolated host-global authority.
/// Its live clients still have to block the executable replacement because
/// the update runs convergence against that workspace after the swap.
#[test]
fn a_live_discovered_workspace_client_blocks_a_local_candidate_update() {
    let install = LocalInstall::new();
    let before = fs::read(&install.installed).expect("installed bytes");
    let candidate = install.candidate("orbit-b");
    let manifest = install.manifest(&candidate, COMMIT_B, "orbit-b.json");
    let workspace_root = install.repo.join(".orbit");
    let workspace_record = workspace_root.join(".generation.lock");
    let workspace_client = GenerationGuard::acquire(&workspace_root, &sha256(&install.installed))
        .expect("pin the discovered workspace authority");
    let record = fs::read(&workspace_record).expect("workspace generation record");

    let output = install.run(
        &candidate,
        &install.install_args(&candidate, &manifest, COMMIT_B),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("upgrade admission refused"), "{stderr}");
    assert_eq!(
        fs::read(&install.installed).expect("installed bytes"),
        before
    );
    assert!(!backup_path(&install.installed).exists());
    assert_eq!(
        fs::read(&workspace_record).expect("generation record"),
        record
    );
    drop(workspace_client);
}

/// A candidate that cannot coordinate with protected clients is refused
/// before it is installed, however correct its manifest.
#[cfg(target_os = "linux")]
#[test]
fn a_candidate_without_the_admission_contract_is_refused_before_replacement() {
    let install = LocalInstall::new();
    let before = fs::read(&install.installed).expect("installed bytes");
    let record = install.generation_record();
    let unprotected = install.builds.join("orbit-unprotected");
    fs::copy("/bin/true", &unprotected).expect("copy a native executable");
    let candidate = install.candidate("orbit-b");
    let manifest = install.manifest_by(&candidate, &unprotected, COMMIT_B, "unprotected.json");
    let output = install.run(
        &candidate,
        &install.install_args(&unprotected, &manifest, COMMIT_B),
    );
    install.assert_refused(
        &output,
        "does not support executable generation admission",
        &before,
        &record,
    );
}

/// Admission and the install lock are held from before the candidate is read
/// until it is pinned: the manifest FIFO holds the updater mid-validation.
/// Meanwhile no client and no second update gets in, and replacing the
/// candidate's pathname changes nothing about what is installed.
#[test]
fn no_client_or_update_enters_mid_validation_and_the_accepted_bytes_are_installed() {
    let install = LocalInstall::new();
    let before = fs::read(&install.installed).expect("installed bytes");
    let candidate = install.candidate("orbit-b");
    let accepted = fs::read(&candidate).expect("candidate bytes");
    let manifest = install.manifest(&candidate, COMMIT_B, "orbit-b.json");
    let fifo = install.builds.join("manifest.fifo");
    mkfifo(&fifo);

    let stdout = install.root.path().join("update.stdout");
    let stderr = install.root.path().join("update.stderr");
    let mut command = install.command(&candidate);
    command
        .args(install.install_args(&candidate, &fifo, COMMIT_B))
        .stdout(fs::File::create(&stdout).expect("stdout"))
        .stderr(fs::File::create(&stderr).expect("stderr"));
    #[cfg(target_os = "linux")]
    let child = orbit_common::test_process::retry_executable_busy(|| command.spawn());
    #[cfg(not(target_os = "linux"))]
    let child = command.spawn();
    let mut child = ReapedChild {
        child: Some(child.expect("spawn update")),
    };
    let mut writer = open_fifo_writer(
        &fifo,
        child.child.as_mut().expect("child"),
        Instant::now() + RENDEZVOUS_DEADLINE,
    );

    // The candidate path now names other bytes.
    let replacement = install.builds.join("orbit-b.next");
    fs::copy(&candidate, &replacement).expect("copy candidate");
    fs::OpenOptions::new()
        .append(true)
        .open(&replacement)
        .and_then(|mut file| file.write_all(b"replaced mid-update"))
        .expect("change replacement");
    fs::rename(&replacement, &candidate).expect("replace the candidate path");

    // A client of the installed build is refused admission, not queued.
    let client = install.run(
        &install.installed,
        &args(&["task".as_ref(), "list".as_ref(), "--json".as_ref()]),
    );
    assert_eq!(client.status.code(), Some(1), "{client:?}");
    assert!(
        String::from_utf8_lossy(&client.stderr).contains("upgrade admission refused"),
        "{client:?}"
    );
    // So is a second update, and the installation is untouched so far.
    let second = install.run(
        &install.installed,
        &install.install_args(&candidate, &manifest, COMMIT_B),
    );
    assert_eq!(second.status.code(), Some(1), "{second:?}");
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("upgrade admission refused"),
        "{second:?}"
    );
    assert_eq!(fs::read(&install.installed).expect("installed"), before);

    writer
        .write_all(&fs::read(&manifest).expect("manifest"))
        .expect("deliver the manifest");
    drop(writer);
    let status = wait_exit(
        child.child.as_mut().expect("child"),
        Instant::now() + RENDEZVOUS_DEADLINE,
    );
    child.child = None;
    let output = Output {
        status,
        stdout: fs::read(&stdout).expect("stdout"),
        stderr: fs::read(&stderr).expect("stderr"),
    };
    assert_success(&output, "the admitted update");
    let updated = report(&output);
    assert_eq!(updated["outcome"], "updated", "{updated}");
    assert_converged(&updated);
    assert_eq!(
        fs::read(&install.installed).expect("installed"),
        accepted,
        "the installed bytes are the ones accepted, not the replaced pathname"
    );
    assert_eq!(
        updated["local_candidate"]["executable_sha256"]["value"],
        sha256(&install.installed)
    );

    // Once the update has released admission, clients run again.
    let after = install.run(
        &install.installed,
        &args(&["task".as_ref(), "list".as_ref(), "--json".as_ref()]),
    );
    assert_success(&after, "a client after the update");
}

/// A convergence failure after the swap reports `needs_recovery` with the
/// exact command that retries this candidate; retrying it converges.
#[test]
fn a_post_swap_failure_needs_recovery_and_the_same_candidate_converges_on_retry() {
    let install = LocalInstall::new();
    let candidate = install.candidate("orbit-b");
    let manifest = install.manifest(&candidate, COMMIT_B, "orbit-b.json");
    let layout = install.repo.join(".orbit/state/layout.version");
    let current_layout = fs::read(&layout).ok();
    fs::create_dir_all(layout.parent().expect("state directory")).expect("state directory");
    fs::write(&layout, "99\n").expect("make the workspace unmigratable");

    let output = install.run(
        &candidate,
        &install.install_args(&candidate, &manifest, COMMIT_B),
    );
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    let failed = report(&output);
    assert_eq!(failed["outcome"], "needs_recovery", "{failed}");
    assert_eq!(failed["replaced"], true);
    assert_eq!(failed["steps"][0]["command"], "migrate --confirm");
    assert_eq!(failed["steps"][0]["status"], "failed");
    let retry = failed["local_candidate"]["retry_command"]
        .as_str()
        .expect("retry command");
    assert!(
        failed["recovery"]
            .as_str()
            .expect("recovery")
            .contains(retry),
        "{failed}"
    );
    assert!(
        retry.contains(COMMIT_B) && retry.contains("--install-target"),
        "{retry}"
    );
    assert_eq!(
        fs::read(&install.installed).expect("installed"),
        fs::read(&candidate).expect("candidate")
    );

    match current_layout {
        Some(marker) => fs::write(&layout, marker).expect("repair the workspace"),
        None => fs::remove_file(&layout).expect("repair the workspace"),
    }
    let retried = install.retry_command(retry);
    assert_success(&retried, "retry the same candidate");
    assert!(
        String::from_utf8_lossy(&retried.stdout)
            .contains("already is the accepted local candidate"),
        "{retried:?}"
    );
    let replay = install.run(
        &install.installed,
        &install.install_args(&candidate, &manifest, COMMIT_B),
    );
    let converged = report(&replay);
    assert_eq!(converged["outcome"], "already_current", "{converged}");
    assert_eq!(converged["replaced"], false);
    assert_converged(&converged);
}

/// Point an executable header at the other supported architecture.
fn retarget(path: &Path) {
    let mut bytes = fs::read(path).expect("read executable");
    if bytes.starts_with(b"\x7fELF") {
        let machine = u16::from_le_bytes([bytes[18], bytes[19]]);
        let other: u16 = if machine == 62 { 183 } else { 62 };
        bytes[18..20].copy_from_slice(&other.to_le_bytes());
    } else {
        let cpu = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let other: u32 = if cpu == 0x0100_0007 {
            0x0100_000c
        } else {
            0x0100_0007
        };
        bytes[4..8].copy_from_slice(&other.to_le_bytes());
    }
    fs::write(path, bytes).expect("write retargeted executable");
}
