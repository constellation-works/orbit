//! Real init and dispatch under a child-only namespace/privilege denial.
//! No host packages, AppArmor profiles, or production probe overrides change.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use orbit_common::test_env;
use serde_json::Value;
use tempfile::TempDir;

use crate::{fixture_crew, git_repo};

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    work: PathBuf,
    bin: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let fixture = Self {
            home: temp.path().join("home"),
            work: temp.path().join("work"),
            bin: temp.path().join("bin"),
            _temp: temp,
        };
        fs::create_dir_all(&fixture.home).unwrap();
        fs::create_dir_all(&fixture.bin).unwrap();
        git_repo::init(&fixture.work);
        fixture
    }

    fn command(&self) -> assert_cmd::Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("orbit"));
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("PATH", &self.bin)
            .env("ORBIT_SKIP_HOST_PREREQUISITES", "0")
            .env_remove("ORBIT_HOME")
            .env_remove("ORBIT_FORMAT")
            .env_remove("RUST_LOG")
            .env_remove("SUDO_UID")
            .env_remove("SUDO_GID")
            .stdin(Stdio::null());
        deny_namespace_and_elevation(&mut command);
        let mut command = assert_cmd::Command::from_std(command);
        command.timeout(Duration::from_secs(30));
        command
    }

    fn init(&self, interactive: bool, format: &[&str]) -> Output {
        let mut command = self.command();
        command.args([
            "init",
            "--machine-name",
            "sandbox-fixture",
            "--task-prefix",
            "SBX",
        ]);
        if !interactive {
            command.arg("--non-interactive");
        }
        command.args(format).output().unwrap()
    }
}

fn instruction(code: u16, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

fn deny_namespace_and_elevation(command: &mut Command) {
    // Both clone and unshare can create Bubblewrap's namespaces. clone3
    // returns ENOSYS so libc can fall back to clone for ordinary threads.
    let filter = [
        instruction(0x20, 0, 0, 0), // load seccomp_data.nr
        instruction(0x15, 6, 0, libc::SYS_unshare as u32),
        instruction(0x15, 6, 0, libc::SYS_clone3 as u32),
        instruction(0x15, 1, 0, libc::SYS_clone as u32),
        instruction(0x06, 0, 0, libc::SECCOMP_RET_ALLOW),
        instruction(0x20, 0, 0, 16), // load low word of args[0]
        instruction(0x45, 1, 0, (libc::CLONE_NEWUSER | libc::CLONE_NEWNS) as u32),
        instruction(0x06, 0, 0, libc::SECCOMP_RET_ALLOW),
        instruction(0x06, 0, 0, libc::SECCOMP_RET_ERRNO | libc::EPERM as u32),
        instruction(0x06, 0, 0, libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32),
    ];
    // SAFETY: only libc syscalls run after fork. The filter is owned by the
    // closure, and the kernel copies it synchronously. These restrictions
    // affect only the disposable CLI child and its descendants; no_new_privs
    // also prevents sudo from acquiring host authority on a supported distro.
    unsafe {
        command.pre_exec(move || {
            let program = libc::sock_fprog {
                len: filter.len() as u16,
                filter: filter.as_ptr().cast_mut(),
            };
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

fn assert_warned_init(output: &Output) -> Value {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(stderr.matches("warning:").count(), 1, "{stderr}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["linux_sandbox"]["status"], "not_ready");
    let reason = result["linux_sandbox"]["reason"].as_str().unwrap();
    assert!(!reason.is_empty(), "preparation must explain its failure");
    assert!(
        stderr.contains(reason),
        "warning must name the reason: {stderr}"
    );
    for remedy in [
        "docs/runbooks/linux-sandbox.md",
        "orbit init --host-prerequisites-only",
        "orbit doctor providers",
        "dispatch stays blocked",
    ] {
        assert!(stderr.contains(remedy), "missing remedy {remedy}: {stderr}");
    }
    result
}

#[test]
fn failed_preparation_still_seeds_init_in_both_modes_and_json_formats() {
    for (interactive, format) in [(true, &["--json"][..]), (false, &["--format", "json"][..])] {
        let fixture = Fixture::new();
        assert_warned_init(&fixture.init(interactive, format));
        let root = fixture.home.join(".orbit");
        let config: toml::Value = fs::read_to_string(root.join("config.toml"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(config["machine"]["name"].as_str(), Some("sandbox-fixture"));
        for path in [
            "skills",
            "resources/activities",
            "resources/jobs",
            "resources/executors",
        ] {
            assert!(root.join(path).is_dir(), "init must seed {path}");
        }
        fixture
            .command()
            .args(["workspace", "init", "--name", "sandbox-fixture"])
            .assert()
            .success();
        assert!(fixture.work.join(".orbit/config.yaml").is_file());
    }
}

#[test]
fn explicit_host_preparation_still_fails_and_does_not_seed_orbit_state() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .args(["init", "--host-prerequisites-only", "--non-interactive"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    // Process logging may create .orbit/logs; host-only preparation must not
    // seed config, identity, workspace assets, or resources.
    for path in ["config.toml", "resources", "skills"] {
        assert!(
            !fixture.home.join(".orbit").join(path).exists(),
            "host-only preparation must not seed {path}"
        );
    }
}

#[test]
fn skipped_preparation_is_structured_for_opt_out_and_custom_root() {
    for custom_root in [false, true] {
        let fixture = Fixture::new();
        let mut command = fixture.command();
        let root = fixture.home.join("custom-root");
        if custom_root {
            command.arg("--root").arg(&root);
        }
        command.arg("init");
        if !custom_root {
            command.arg("--skip-host-prerequisites");
        }
        let output = command
            .args([
                "--non-interactive",
                "--machine-name",
                "skip-host",
                "--task-prefix",
                "SKP",
                "--json",
            ])
            .assert()
            .success()
            .get_output()
            .clone();
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["linux_sandbox"]["status"], "skipped");
        assert!(
            !result["linux_sandbox"]["reason"]
                .as_str()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn linux_bwrap_dispatch_after_warned_init_refuses_to_launch_the_provider() {
    let fixture = Fixture::new();
    assert_warned_init(&fixture.init(false, &["--json"]));
    let root = fixture.home.join(".orbit");
    fixture_crew::configure_sol(&root);
    std::os::unix::fs::symlink("/usr/bin/git", fixture.bin.join("git")).unwrap();
    let marker = fixture.home.join("provider-launched");
    let provider = fixture.bin.join("codex");
    fs::write(
        &provider,
        format!("#!/bin/sh\n: > '{}'\nexit 99\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o755)).unwrap();
    fixture
        .command()
        .args(["workspace", "init", "--name", "dispatch-fixture"])
        .assert()
        .success();
    let executor: serde_yaml::Value = serde_yaml::from_str(
        &fs::read_to_string(root.join("resources/executors/codex.yaml")).unwrap(),
    )
    .unwrap();
    assert_eq!(executor["spec"]["sandbox"].as_str(), Some("linux-bwrap"));
    let doctor = fixture
        .command()
        .args(["doctor", "providers", "--json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let providers: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    let codex = providers
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["name"] == "codex")
        .unwrap();
    assert_eq!(codex["cli_available"], true, "{codex}");
    assert_eq!(codex["sandbox_ready"], false, "{codex}");
    assert_eq!(codex["allow_fallback"], false, "{codex}");
    fs::write(root.join("resources/jobs/sandbox_probe.yaml"), "schemaVersion: 2\nkind: Job\nmetadata:\n  name: sandbox_probe\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: probe\n      spec:\n        type: agent_loop\n        description: Sandbox refusal probe\n        instruction: Return the fixture response\n        provider: codex\n        backend: cli\n        wall_clock_timeout_seconds: 5\n").unwrap();
    let output = fixture
        .command()
        .args([
            "run",
            "job",
            "sandbox_probe",
            "--input",
            "crew=sol",
            "--wait",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["state"], "failed", "{result}");
    let error = result.to_string();
    assert!(
        error.contains("Bubblewrap") || error.contains("namespace"),
        "dispatch must fail at the sandbox check: {result}"
    );
    assert!(
        !marker.exists(),
        "fail-closed dispatch must never launch the provider"
    );
}
