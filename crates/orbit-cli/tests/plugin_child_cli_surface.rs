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
//! callback session bound to *this* process, and `orbit` spawned as its child,
//! which is how the host-issued credential reaches a backend descendant in
//! production. The child is identified by ancestry as well as by the token, so
//! neither dropping `ORBIT_PLUGIN_CALLBACK` nor keeping it changes the answer.

#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;

use orbit_common::process::ancestry::process_start_key;
use tempfile::TempDir;

/// The refusal the CLI surface guard emits. Matched on its stable clause
/// rather than the whole sentence so wording can be improved without a
/// green-to-red test.
const SURFACE_REFUSAL: &str = "a plugin backend reaches Orbit only through `orbit tool run";

/// An Orbit root holding one live callback session, bound to this process.
///
/// Binding to the test process rather than to the spawned child is what makes
/// the fixture realistic *and* race-free: a backend descendant is recognized
/// through `getppid`, so every `orbit` this test spawns resolves as a plugin
/// child without the test having to know a pid before it forks.
struct PluginChildFixture {
    _temp: TempDir,
    root: PathBuf,
    token: String,
}

impl PluginChildFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("fixture tempdir");
        let root = temp.path().join("orbit-root");
        let token = "a".repeat(64);
        let sessions = root.join("state/plugin-callbacks");
        std::fs::create_dir_all(&sessions).expect("create callback session directory");

        let pid = std::process::id();
        let starttime = process_start_key(pid)
            .expect("read this process's start time")
            .starttime;
        std::fs::write(
            sessions.join(&token),
            serde_json::json!({
                "schema_version": 2,
                "plugin": "graph",
                "version": "0.4.1",
                "manifest_digest": "0".repeat(64),
                // The ceiling the host would have minted for this child. The
                // surface guard fires before any ceiling is consulted, but a
                // record without one is not a session at all [ORB-12801], and
                // the refusal under test is the guard's, not the credential
                // check's.
                "effective_tools": ["orbit.search", "orbit.task.list"],
                "pid": pid,
                "starttime": starttime,
            })
            .to_string(),
        )
        .expect("write callback session record");

        Self {
            _temp: temp,
            root,
            token,
        }
    }

    /// `orbit --root <fixture> …`, run the way a backend descendant runs it.
    fn orbit(&self, args: &[&str]) -> std::process::Output {
        let mut command = cargo_bin_cmd!("orbit");
        command
            .env("ORBIT_PLUGIN_CALLBACK", &self.token)
            .env("ORBIT_PLUGIN", "graph")
            // A backend cannot widen its reach by claiming the allowlist is
            // wider; the recorded manifest is the only source that counts.
            .env("ORBIT_ALLOWED_TOOLS", "orbit.task.list,orbit.search")
            .arg("--root")
            .arg(&self.root)
            .args(args);
        command.output().expect("run orbit as a plugin child")
    }
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

/// The granted path stays open. `orbit tool run` is not refused by the surface
/// guard; it proceeds to the callback allowlist, which is the gate that
/// decides whether *this* plugin may call *that* tool. Here the fixture root
/// records no plugin at all, so the allowlist refuses it by name — which is
/// exactly the proof that the call got past the surface guard.
#[test]
fn the_granted_tool_call_path_is_not_refused_by_the_cli_surface_guard() {
    let fixture = PluginChildFixture::new();

    let output = fixture.orbit(&["tool", "run", "orbit.task.list", "--input", "{}"]);
    let stderr = stderr_of(&output);
    assert!(
        !stderr.contains(SURFACE_REFUSAL),
        "`orbit tool run` is a callback entry point and must reach the allowlist: {stderr}"
    );
}

/// An ordinary caller — no session record for its process — is untouched.
#[test]
fn an_ordinary_caller_is_not_gated_by_the_cli_surface_guard() {
    let temp = tempfile::tempdir().expect("fixture tempdir");
    let root: &Path = temp.path();

    let mut command = cargo_bin_cmd!("orbit");
    command
        .env_remove("ORBIT_PLUGIN_CALLBACK")
        .env_remove("ORBIT_PLUGIN")
        .arg("--root")
        .arg(root)
        .args(["workspace", "list", "--format", "json"]);
    let output = command.output().expect("run orbit as an ordinary caller");
    let stderr = stderr_of(&output);
    assert!(
        !stderr.contains(SURFACE_REFUSAL),
        "a caller with no callback session must not be treated as a plugin child: {stderr}"
    );
}
