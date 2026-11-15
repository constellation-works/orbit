//! ORB-12876: a plugin backend cannot read governed data through a plain CLI
//! command.
//!
//! `permissions.orbit_tools` is enforced on `orbit tool run` and MCP
//! `tools/call` only. Every other CLI command used to reach governed data
//! without consulting it, so a plugin granted nothing at all could still run
//! `orbit workspace list --format json` or `orbit run show <id>` — the two
//! reads orbit-graph actually performs — and read whatever the CLI exposes.
//!
//! This exercises the real binary as a recognized plugin child: a live
//! callback session, and `orbit` spawned holding the session record open on an
//! inherited descriptor, which is how the host-issued credential reaches a
//! backend descendant in production [ORB-12841]. Nothing in the child's
//! environment decides it — the record on the descriptor does — so the
//! fixture also keeps the retired `ORBIT_PLUGIN_CALLBACK` variable set to
//! show it changes nothing.

// The callback credential reaches the child on an inherited descriptor,
// which only Unix supports.
#![cfg(unix)]
#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;

use orbit_common::process::ancestry::process_start_key;
use orbit_common::test_env;
use tempfile::TempDir;

/// The refusal the CLI surface guard emits. Matched on its stable clause
/// rather than the whole sentence so wording can be improved without a
/// green-to-red test.
const SURFACE_REFUSAL: &str = "a plugin backend reaches Orbit only through `orbit tool run";

/// An Orbit root holding one live callback session, with the session record
/// open on a descriptor every spawned `orbit` inherits.
///
/// Holding the descriptor open in the test process rather than binding a pid
/// is what makes the fixture realistic *and* race-free: the credential is the
/// record, not the process tree, so the test hands it down without having to
/// know a pid before it forks.
struct DisposableCliFixture {
    _temp: TempDir,
    home: PathBuf,
    work: PathBuf,
    root: PathBuf,
}

impl DisposableCliFixture {
    fn new(machine_name: &str, workspace_name: &str) -> Self {
        let temp = tempfile::tempdir().expect("fixture tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        let root = home.join(".orbit");
        std::fs::create_dir_all(&home).expect("create fixture home");
        crate::git_repo::init(&work);

        let mut init_cmd = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            init_cmd.env_remove(name);
        });
        init_cmd
            .current_dir(&work)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .arg("--root")
            .arg(&root)
            .args([
                "init",
                "--non-interactive",
                "--machine-name",
                machine_name,
                "--task-prefix",
                "TST",
            ]);
        let output = init_cmd.output().expect("init fixture root");
        assert!(
            output.status.success(),
            "init fixture root: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let mut ws_cmd = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            ws_cmd.env_remove(name);
        });
        ws_cmd
            .current_dir(&work)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .args(["workspace", "init", "--name", workspace_name]);
        let output = ws_cmd.output().expect("init fixture workspace");
        assert!(
            output.status.success(),
            "init fixture workspace: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        Self {
            _temp: temp,
            home,
            work,
            root,
        }
    }

    fn orbit(&self, args: &[&str]) -> std::process::Output {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .env_remove("ORBIT_PLUGIN_CALLBACK")
            .env_remove("ORBIT_PLUGIN_CALLBACK_FD")
            .env_remove("ORBIT_PLUGIN")
            .env_remove("ORBIT_ALLOWED_TOOLS")
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .arg("--root")
            .arg(&self.root)
            .args(args);
        command.output().expect("run orbit as an ordinary caller")
    }
}

/// An Orbit root holding one live callback session, with the session record
/// open on a descriptor every spawned `orbit` inherits.
///
/// Holding the descriptor open in the test process rather than binding a pid
/// is what makes the fixture realistic *and* race-free: the credential is the
/// record, not the process tree, so the test hands it down without having to
/// know a pid before it forks.
struct PluginChildFixture {
    base: DisposableCliFixture,
    root: PathBuf,
    token: String,
    /// The host's open handle on the record. Inheritable, so the spawned
    /// `orbit` holds the very same descriptor.
    credential: std::fs::File,
}

impl PluginChildFixture {
    fn new() -> Self {
        Self::from_base(DisposableCliFixture::new(
            "plugin-child-host",
            "child-fixture-ws",
        ))
    }

    fn from_base(base: DisposableCliFixture) -> Self {
        let root = base.root.clone();
        let sessions = root.join("state/plugin-callbacks");
        std::fs::create_dir_all(&sessions).expect("create callback session directory");

        let pid = std::process::id();
        let starttime = process_start_key(pid)
            .expect("read this process's start time")
            .starttime;
        let token = "a".repeat(64);
        let record = sessions.join(&token);
        std::fs::write(
            &record,
            serde_json::json!({
                "schema_version": 5,
                "workspace_root": base.work,
                "plugin": "graph",
                "version": "0.4.1",
                "manifest_digest": "0".repeat(64),
                // The ceiling the host would have minted for this child. The
                // surface guard fires before any ceiling is consulted, but a
                // record without one is not a session at all [ORB-12801], and
                // the refusal under test is the guard's, not the credential
                // check's.
                "effective_tools": ["orbit.search", "orbit.task.list"],
                // The record names itself: a child holding the descriptor
                // checks that this name resolves to the very inode it holds.
                "token": token,
                "pid": pid,
                "starttime": starttime,
            })
            .to_string(),
        )
        .expect("write callback session record");
        let credential = inheritable_credential(&record);

        Self {
            base,
            root,
            token,
            credential,
        }
    }

    /// `orbit --root <fixture> …`, configured as a backend descendant.
    fn command(&self, args: &[&str]) -> assert_cmd::Command {
        let mut command = self.command_without_root();
        command.arg("--root").arg(&self.root).args(args);
        command
    }

    /// A descendant using HOME for its root, so id-only routing stays enabled.
    fn command_without_root(&self) -> assert_cmd::Command {
        use std::os::fd::AsRawFd;

        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.base.work)
            .env("HOME", &self.base.home)
            .env("USERPROFILE", &self.base.home)
            // The credential: the record the host wrote, open on a descriptor
            // this process is handing down. A test cannot dictate a
            // process-wide number, so it names the one it got — which is what
            // the variable exists for.
            .env(
                "ORBIT_PLUGIN_CALLBACK_FD",
                self.credential.as_raw_fd().to_string(),
            )
            .env("ORBIT_PLUGIN_CALLBACK", &self.token)
            .env("ORBIT_PLUGIN", "graph")
            // A backend cannot widen its reach by claiming the allowlist is
            // wider; the recorded manifest is the only source that counts.
            .env("ORBIT_ALLOWED_TOOLS", "orbit.task.list,orbit.search");
        command
    }

    fn orbit(&self, args: &[&str]) -> std::process::Output {
        self.command(args)
            .output()
            .expect("run orbit as a plugin child")
    }
}

/// Open `record` on a descriptor a spawned child inherits.
///
/// Rust marks every descriptor it opens close-on-exec, which is what keeps an
/// ordinary child from inheriting a handle nobody meant to give it. The host
/// clears the flag deliberately for exactly one descriptor; so does this
/// fixture.
fn inheritable_credential(record: &Path) -> std::fs::File {
    use std::os::fd::AsRawFd;

    let file = std::fs::File::open(record).expect("open the callback session record");
    // SAFETY: `file` owns the descriptor for the whole call, and clearing
    // close-on-exec only changes what `exec` does with it.
    let cleared = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, 0) };
    assert!(
        cleared >= 0,
        "clear close-on-exec on the callback credential: {}",
        std::io::Error::last_os_error()
    );
    file
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The two reads named in the gap report, refused at the CLI chokepoint.
#[test]
fn a_plugin_child_cannot_read_governed_data_through_plain_cli_commands() {
    let fixture = PluginChildFixture::new();

    for args in [
        vec!["workspace", "list", "--format", "json"],
        vec!["run", "show", "jrun-20260101-0000-aa"],
    ] {
        let output = fixture.orbit(&args);
        let stderr = stderr_of(&output);
        assert!(
            !output.status.success(),
            "`orbit {}` must fail for a plugin child: {stderr}",
            args.join(" ")
        );
        assert!(
            stderr.contains(SURFACE_REFUSAL) && stderr.contains("plugin 'graph'"),
            "`orbit {}` must be refused as a plugin child, got: {stderr}",
            args.join(" ")
        );
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains("workspace_id"),
            "`orbit {}` must not emit records before the refusal",
            args.join(" ")
        );
    }
}

/// The refusal covers the whole surface, not a list of known-sensitive reads:
/// a command added tomorrow is closed by default.
#[test]
fn the_refusal_is_the_whole_cli_surface_including_self_upgrade() {
    let fixture = PluginChildFixture::new();

    for args in [
        vec!["task", "list"],
        // `audit` declares no audit identity, so this also covers the
        // refusal's unnamed-command wording.
        vec!["audit", "prune", "--older-than", "30d"],
        vec!["plugin", "list"],
        vec!["update"],
    ] {
        let stderr = stderr_of(&fixture.orbit(&args));
        assert!(
            stderr.contains(SURFACE_REFUSAL),
            "`orbit {}` must be refused as a plugin child, got: {stderr}",
            args.join(" ")
        );
    }
}

/// A plugin backend must not set, list or remove secrets: they are the
/// operator's, and a backend that could `set` one could plant a credential
/// for another plugin — or read which ones exist. Every `secret` verb is
/// refused as `policy_denied`, before anything reads stdin or the store.
#[test]
fn a_plugin_child_cannot_manage_plugin_secrets() {
    let fixture = PluginChildFixture::new();

    for args in [
        vec!["plugin", "secret", "set", "graph", "token"],
        vec!["plugin", "secret", "list", "graph"],
        vec!["plugin", "secret", "rm", "graph", "token"],
    ] {
        let mut json_args = args.clone();
        json_args.extend(["--format", "json"]);
        let output = fixture.orbit(&json_args);
        assert!(
            !output.status.success(),
            "`orbit {}` must fail for a plugin child",
            args.join(" ")
        );
        let stderr = stderr_of(&output);
        let error: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap_or_else(|err| {
            panic!("`orbit {}` JSON error ({err}): {stderr}", args.join(" "))
        });
        assert_eq!(
            error["code"],
            "policy_denied",
            "`orbit {}` must be refused as policy, got: {stderr}",
            args.join(" ")
        );
        assert!(
            !fixture.root.join("state/plugin-secrets").exists(),
            "a refused secret verb must not touch the store"
        );
    }
}

/// The granted path stays open. `orbit tool run` is not refused by the surface
/// guard; it proceeds to the callback allowlist, which is the gate that
/// decides whether *this* plugin may call *that* tool. Here the fixture root
/// records no plugin at all, so the allowlist refuses it by name — which is
/// exactly the proof that the call got past the surface guard.
#[test]
fn the_granted_tool_call_path_is_not_refused_by_the_cli_surface_guard() {
    let fixture = PluginChildFixture::new();

    let output = fixture.orbit(&["tool", "run", "orbit.task.list", "--input", "{}"]);
    assert!(
        !output.status.success(),
        "`orbit tool run` without granted tool must fail at allowlist check"
    );
    let stderr = stderr_of(&output);
    assert!(
        !stderr.contains(SURFACE_REFUSAL),
        "`orbit tool run` is a callback entry point and must reach the allowlist: {stderr}"
    );
    let error: serde_json::Value = stderr
        .lines()
        .find_map(|line| serde_json::from_str(line.trim()).ok())
        .unwrap_or_else(|| panic!("expected JSON error on stderr, got: {stderr}"));
    assert_eq!(
        error["code"], "policy_denied",
        "`orbit tool run` must be refused as policy, got: {stderr}"
    );
    let message = error["error"]
        .as_str()
        .expect("error field should be a string");
    assert!(
        message.contains(
            "tool 'orbit.task.list' is not in plugin 'graph''s granted orbit_tools allowlist"
        ),
        "expected plugin/tool allowlist refusal, got: {message}"
    );
}

/// An ordinary caller — no session record for its process — is untouched.
#[test]
fn an_ordinary_caller_is_not_gated_by_the_cli_surface_guard() {
    let fixture = DisposableCliFixture::new("ordinary-caller-host", "ordinary-fixture-ws");

    let output = fixture.orbit(&["workspace", "list", "--format", "json"]);
    let stderr = stderr_of(&output);
    assert!(
        output.status.success(),
        "`orbit workspace list` must succeed for an ordinary caller: {stderr}"
    );
    assert!(
        !stderr.contains(SURFACE_REFUSAL),
        "a caller with no callback session must not be treated as a plugin child: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let list: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("expected JSON array on stdout ({err}), got: {stdout}"));
    let workspaces = list.as_array().expect("workspace list emits a JSON array");
    assert!(
        workspaces
            .iter()
            .any(|ws| ws["name"] == "ordinary-fixture-ws"),
        "workspace list must contain the fixture workspace, got: {stdout}"
    );
}

/// A remote selector must be refused before a backend descendant tries SSH,
/// with a warning retained in the operational log as well as the callback error.
#[test]
fn a_plugin_child_remote_selector_is_refused_before_ssh_with_a_warning() {
    assert_remote_callback_refused(
        &[],
        "orbit.task.list",
        r#"{"workspace":"hm_1111111111111111/ws_remote","limit":1}"#,
        "hm_1111111111111111/ws_remote",
    );
}

#[test]
fn a_plugin_child_remote_host_flag_is_refused_before_ssh_with_a_warning() {
    assert_remote_callback_refused(
        // An overlapping local name gets past the local workspace guard,
        // exposing the remote discovery attempt as well as the wrong error.
        &["--host", "remote", "--workspace", "child-fixture-ws"],
        "orbit.task.list",
        "{}",
        "--host remote --workspace child-fixture-ws",
    );
}

#[test]
fn a_plugin_child_remote_task_id_is_refused_before_ssh_with_a_warning() {
    assert_remote_callback_refused(&[], "orbit.task.show", r#"{"id":"REM-1"}"#, "REM-1");
}

fn assert_remote_callback_refused(flags: &[&str], tool: &str, input: &str, destination: &str) {
    use std::os::unix::fs::PermissionsExt;

    let fixture = PluginChildFixture::new();
    let global = fixture.root.clone();
    std::fs::write(
        global.join("hosts.toml"),
        r#"schema_version = 1
[[hosts]]
name = "remote"
machine_id = "hm_1111111111111111"
ssh = "remote-fixture"
task_prefix = "REM"
"#,
    )
    .expect("register remote host");
    let bin = fixture.base.home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let ssh = bin.join("ssh");
    std::fs::write(
        &ssh,
        "#!/bin/sh\nprintf attempted > \"$SSH_SENTINEL\"\nexit 42\n",
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let sentinel = fixture.base.home.join("ssh-attempted");
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let output = fixture
        .command_without_root()
        .args(["tool", "run", tool, "--input", input])
        .args(flags)
        .env("PATH", path)
        .env("SSH_SENTINEL", &sentinel)
        .env("RUST_LOG", "warn")
        .output()
        .expect("remote callback");
    let stderr = stderr_of(&output);
    assert!(
        !output.status.success(),
        "remote callback must fail: {stderr}"
    );
    assert!(
        !sentinel.exists(),
        "remote callback attempted SSH: {stderr}"
    );
    let error: serde_json::Value = stderr
        .lines()
        .find_map(|line| serde_json::from_str(line).ok())
        .unwrap_or_else(|| panic!("callback must return a structured error: {stderr}"));
    assert_eq!(error["code"], "policy_denied", "{stderr}");
    let message = error["error"].as_str().unwrap();
    assert!(
        message.contains("cross-host callbacks are not supported from confined plugins")
            && message.contains(destination),
        "callback must name cause and destination: {stderr}"
    );
    let log = std::fs::read_to_string(global.join("state/logs/orbit.jsonl")).unwrap();
    let warning = log
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|row| row["level"] == "WARN" && row["fields"]["workspace"] == destination)
        .unwrap_or_else(|| panic!("missing callback warning: {log}"));
    assert_eq!(warning["fields"]["plugin"], "graph");
    assert_eq!(warning["fields"]["tool"], tool);
    assert_eq!(warning["fields"]["code"], "policy_denied");
    assert_eq!(warning["fields"]["message"], message);
}

/// A real Landlock boundary denies the second checkout's store. Refuse it
/// before opening that store, even if the descendant changes cwd.
#[cfg(target_os = "linux")]
#[test]
fn a_confined_callback_to_another_local_workspace_is_refused_before_store_open() {
    use orbit_exec::{EnvironmentMode, ExecRequest, LandlockBoundary, StdinMode};
    use std::os::fd::AsRawFd;

    let probe = orbit_exec::probe_landlock();
    assert!(
        probe.available,
        "Landlock required for callback regression: {}",
        probe.detail
    );
    let base = DisposableCliFixture::new("plugin-child-host", "child-fixture-ws");
    let other = base.home.join("other-work");
    crate::git_repo::init(&other);
    let mut init = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        init.env_remove(name);
    });
    let initialized = init
        .current_dir(&other)
        .env("HOME", &base.home)
        .env("USERPROFILE", &base.home)
        .args(["workspace", "init", "--name", "second-local-ws"])
        .output()
        .unwrap();
    assert!(initialized.status.success(), "{}", stderr_of(&initialized));

    let fixture = PluginChildFixture::from_base(base);
    let input = serde_json::json!({"workspace": "second-local-ws"}).to_string();
    let mut environment: std::collections::BTreeMap<_, _> = std::env::vars().collect();
    test_env::clear_inherited_authority(|name| {
        environment.remove(name);
    });
    environment.insert(
        "HOME".into(),
        fixture.base.home.to_string_lossy().into_owned(),
    );
    environment.insert(
        "USERPROFILE".into(),
        fixture.base.home.to_string_lossy().into_owned(),
    );
    environment.insert(
        "ORBIT_PLUGIN_CALLBACK_FD".into(),
        fixture.credential.as_raw_fd().to_string(),
    );
    environment.insert("ORBIT_PLUGIN_CALLBACK".into(), fixture.token.clone());
    environment.insert("ORBIT_PLUGIN".into(), "graph".into());
    environment.insert("RUST_LOG".into(), "warn".into());
    let mut request = ExecRequest {
        program: env!("CARGO_BIN_EXE_orbit").to_string(),
        args: vec![
            "--root".into(),
            fixture.root.to_string_lossy().into_owned(),
            "tool".into(),
            "run".into(),
            "orbit.task.list".into(),
            "--input".into(),
            input,
        ],
        current_dir: Some(fixture.base.work.to_string_lossy().into_owned()),
        timeout_ms: Some(30_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::ClearAndSet(environment.into_iter().collect()),
        debug: false,
    };
    // The product markers above the roots are granted as the plugin sandbox
    // grants them, so the refusal under test is the workspace policy's and not
    // a marker read the boundary happens to deny (a fixture under another
    // Orbit tree, as in a managed worktree lane) [ORB-23682].
    let mut read = vec![
        fixture.root.clone(),
        fixture.base.work.clone(),
        other.clone(),
    ];
    read.extend(orbit_tools::plugin::orbit_product_markers_above_roots([
        fixture.root.clone(),
        fixture.base.work.join(".orbit"),
        other.join(".orbit"),
    ]));
    let boundary = LandlockBoundary {
        read,
        read_denies: vec![other.join(".orbit")],
        write: vec![fixture.root.clone(), fixture.base.work.join(".orbit")],
        ..Default::default()
    };
    // The same checkout must still reach the ordinary callback allowlist.
    let original_args = request.args.clone();
    *request.args.last_mut().unwrap() =
        serde_json::json!({"workspace": "child-fixture-ws"}).to_string();
    let descriptor = fixture.credential.as_raw_fd();
    let same = orbit_exec::spawn_under_linux_landlock_boundary(
        &request,
        &boundary,
        &[orbit_exec::InheritedFd {
            source: descriptor,
            target: descriptor,
        }],
    )
    .unwrap()
    .wait_with_output()
    .unwrap();
    let same_stderr = stderr_of(&same);
    let same_error: serde_json::Value = same_stderr
        .lines()
        .find_map(|line| serde_json::from_str(line).ok())
        .unwrap();
    assert_eq!(same_error["code"], "policy_denied", "{same_stderr}");
    assert!(
        same_error["error"].as_str().unwrap().contains("allowlist"),
        "{same_stderr}"
    );
    request.args = original_args;
    for cwd in [&fixture.base.work, &other] {
        request.current_dir = Some(cwd.to_string_lossy().into_owned());
        let child = orbit_exec::spawn_under_linux_landlock_boundary(
            &request,
            &boundary,
            &[orbit_exec::InheritedFd {
                source: descriptor,
                target: descriptor,
            }],
        )
        .unwrap();
        let output = child.wait_with_output().unwrap();
        let stderr = stderr_of(&output);
        assert!(!output.status.success(), "{stderr}");
        assert!(output.stdout.is_empty(), "refused callback emitted data");
        let error: serde_json::Value = stderr
            .lines()
            .find_map(|line| serde_json::from_str(line).ok())
            .unwrap_or_else(|| panic!("structured callback refusal: {stderr}"));
        assert_eq!(
            error["code"], "callback_workspace_outside_sandbox",
            "{stderr}"
        );
        assert!(
            error["error"].as_str().unwrap().contains("second-local-ws"),
            "{stderr}"
        );
    }
    let log = std::fs::read_to_string(fixture.root.join("state/logs/orbit.jsonl")).unwrap();
    assert!(
        log.lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .any(|row| row["level"] == "WARN"
                && row["fields"]["code"] == "callback_workspace_outside_sandbox"
                && row["fields"]["plugin"] == "graph"
                && row["fields"]["tool"] == "orbit.task.list"),
        "{log}"
    );

    // A host-issued unconfined profile retains the existing local route;
    // it reaches the tool allowlist instead of this sandbox refusal.
    let record_path = fixture
        .root
        .join("state/plugin-callbacks")
        .join(&fixture.token);
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
    record.as_object_mut().unwrap().remove("workspace_root");
    record["schema_version"] = serde_json::json!(4);
    std::fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
    let legacy = fixture.orbit(&[
        "tool",
        "run",
        "orbit.task.list",
        "--input",
        "{\"workspace\":\"child-fixture-ws\"}",
    ]);
    let legacy_stderr = stderr_of(&legacy);
    let legacy_error: serde_json::Value = legacy_stderr
        .lines()
        .find_map(|line| serde_json::from_str(line).ok())
        .unwrap();
    assert_eq!(
        legacy_error["code"], "callback_workspace_outside_sandbox",
        "{legacy_stderr}"
    );
    assert!(
        legacy_error["error"].as_str().unwrap().contains("restart"),
        "{legacy_stderr}"
    );

    record["unconfined"] = serde_json::json!(true);
    std::fs::write(record_path, serde_json::to_vec(&record).unwrap()).unwrap();
    let unconfined = fixture.orbit(&[
        "tool",
        "run",
        "orbit.task.list",
        "--input",
        "{\"workspace\":\"second-local-ws\"}",
    ]);
    let stderr = stderr_of(&unconfined);
    let error: serde_json::Value = stderr
        .lines()
        .find_map(|line| serde_json::from_str(line).ok())
        .unwrap();
    assert_eq!(error["code"], "policy_denied", "{stderr}");
    assert!(
        error["error"].as_str().unwrap().contains("allowlist"),
        "{stderr}"
    );
}
