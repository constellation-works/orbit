//! A confined decision-channel plugin runs its digest and submit callbacks
//! end to end under the real plugin sandbox (design
//! `docs/design/human-decision-channel/2_design.md` §3.2–§3.3).
//!
//! The channel store holds every channel's token key, so every plugin child is
//! denied it, and before the host route a sandboxed backend's two callbacks
//! were refused as not visible. Everything here runs the built `orbit`: the
//! operator calls an installed plugin tool, the default-sandboxed backend runs
//! the nested `orbit tool run orbit.decision.*` itself, and the host that
//! spawned it answers the call.
#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use orbit_core::runtime::OrbitRuntime;
use orbit_core::runtime::context::ActorIdentity;
use orbit_core::test_support::EngineHost;
use orbit_core::test_support::fixture_engine::{RunLifecycle, TaskAutomationUpdate};
use orbit_types::task::{DecisionAnswer, DecisionBinding, DecisionGate, DecisionRequest};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::git_repo;

const CHILD_TIMEOUT: Duration = Duration::from_secs(120);
const CHANNEL: &str = "approvals";
const SENDER: &str = "alex@example.com";

/// Runs the callback its tool name selects and leaves the nested `orbit`'s
/// exit status, stdout and stderr in its own state directory; `probe` reports
/// whether the channel store is readable from inside the sandbox.
const BACKEND: &str = r#"#!/bin/sh
cat >/dev/null
st="$ORBIT_PLUGIN_STATE"
case "$ORBIT_TOOL_NAME" in
  *.digest) orbit tool run orbit.decision.digest --input "{\"channel\":\"approvals\"}" >"$st/out" 2>"$st/err"; echo $? >"$st/status" ;;
  *.submit) orbit tool run orbit.decision.submit --input-file "$st/submit.json" >"$st/out" 2>"$st/err"; echo $? >"$st/status" ;;
  *.probe)
    seen=readable
    cat "$HOME/.orbit/state/decision-channels/channels.json" >/dev/null 2>&1 || seen=denied
    ls "$HOME/.orbit/state/decision-channels" >/dev/null 2>&1 && seen="$seen,listed"
    echo "$seen" >"$st/status" ;;
esac
printf '{"ok":true,"output":{}}\n'
"#;

struct Host {
    _temp: TempDir,
    root: PathBuf,
    home: PathBuf,
    work: PathBuf,
}

/// What one nested callback left behind.
struct Callback {
    status: String,
    stdout: String,
    stderr: String,
}

impl Host {
    fn new() -> Self {
        let temp = tempfile::tempdir_in(test_env::canonical_temp_dir()).expect("tempdir");
        let root = temp.path().to_path_buf();
        let home = root.join("home");
        let work = root.join("work");
        std::fs::create_dir_all(&home).expect("home");
        git_repo::init(&work);
        let host = Self {
            _temp: temp,
            root,
            home,
            work,
        };
        host.ok(&[
            "init",
            "--non-interactive",
            "--machine-name",
            "decision-sandbox",
            "--task-prefix",
            "TST",
        ]);
        host.ok(&["workspace", "init", "--name", "decision-sandbox"]);
        host.ok(&["config", "set", "decision_channel.enabled", "true"]);
        host
    }

    /// `orbit` pinned to this host with no inherited authority. The built
    /// binary's directory leads `PATH`, which the backend inherits, so the
    /// confined child can execute the same `orbit`.
    fn orbit(&self, env: &[(&str, &str)]) -> assert_cmd::Command {
        let bin_dir = Path::new(env!("CARGO_BIN_EXE_orbit"))
            .parent()
            .expect("binary directory");
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        for name in [
            "AGENT_RUN_ID",
            "ORBIT_ACTIVITY_TOOLS",
            "ORBIT_TASK_ACTOR_KIND",
        ] {
            command.env_remove(name);
        }
        command
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
            .timeout(CHILD_TIMEOUT);
        for (name, value) in env {
            command.env(name, value);
        }
        command
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.orbit(env).args(args).output().expect("run orbit")
    }

    fn ok(&self, args: &[&str]) -> Output {
        let output = self.run(args, &[]);
        assert!(output.status.success(), "orbit {args:?}: {}", text(&output));
        output
    }

    fn state(&self, namespace: &str) -> PathBuf {
        self.home.join(".orbit/state/plugins").join(namespace)
    }

    /// A default-sandboxed plugin granted both decision callbacks and its
    /// own state for the callback's output.
    fn add_plugin(&self, namespace: &str) {
        let tree = self
            .root
            .join("sources")
            .join(namespace)
            .join(".orbit-plugin");
        std::fs::create_dir_all(tree.join("bin")).expect("plugin tree");
        let backend = tree.join("bin/backend.sh");
        std::fs::write(&backend, BACKEND).expect("backend");
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755))
            .expect("chmod backend");
        std::fs::write(
            tree.join("plugin.yaml"),
            format!(
                "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {namespace}\n  version: 0.1.0\n  \
                 description: Decision channel transport fixture.\nspec:\n  permissions:\n    \
                 orbit_tools: [orbit.decision.digest, orbit.decision.submit]\n    fs:\n      \
                 write: [\"{{{{plugin_state}}}}\"]\n  secrets:\n    - name: channel_key\n      \
                 description: Decision-channel token key managed by Orbit.\n      rotatable: true\n  \
                 backend:\n    type: exec\n    command: bin/backend.sh\n  tools:\n    - name: digest\n      \
                 description: Run the digest.\n      execution_kind: mutating\n      mcp_scope: workspace\n    \
                 - name: submit\n      description: Submit an answer.\n      execution_kind: mutating\n      \
                 mcp_scope: workspace\n    - name: probe\n      description: Probe the channel store.\n      \
                 execution_kind: read_only\n      mcp_scope: workspace\n"
            ),
        )
        .expect("manifest");
        let source = tree
            .parent()
            .expect("source")
            .to_str()
            .expect("utf8 source");
        self.ok(&[
            "plugin",
            "add",
            source,
            "--enable",
            "--grant",
            "orbit_tools,fs",
        ]);
    }

    /// Call `<namespace>.<verb>` as the operator and read what its backend's
    /// nested callback left behind.
    fn callback(&self, namespace: &str, verb: &str) -> Callback {
        let state = self.state(namespace);
        for file in ["status", "out", "err"] {
            let _ = std::fs::remove_file(state.join(file));
        }
        let tool = format!("{namespace}.{verb}");
        let output = self.run(
            &["tool", "run", &tool, "--input", "{}"],
            &[("ORBIT_OPERATOR", "1")],
        );
        assert!(output.status.success(), "{tool}: {}", text(&output));
        let read = |file: &str| std::fs::read_to_string(state.join(file)).unwrap_or_default();
        Callback {
            status: read("status").trim().to_string(),
            stdout: read("out"),
            stderr: read("err"),
        }
    }

    fn task_status(&self, id: &str) -> String {
        let shown: Value = serde_json::from_slice(&self.ok(&["task", "show", id, "--json"]).stdout)
            .expect("task show JSON");
        shown["status"].as_str().expect("status").to_string()
    }
}

fn text(output: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The documented token contract: the first 26 characters of unpadded
/// Base32 of `HMAC-SHA256(key, request_id|task_id|epoch|expires_at)`.
fn channel_token(key_hex: &str, epoch: u64, request: &Value) -> String {
    let key: Vec<u8> = (0..key_hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&key_hex[at..at + 2], 16).expect("hex key"))
        .collect();
    let expires =
        chrono::DateTime::parse_from_rfc3339(request["expires_at"].as_str().expect("expires_at"))
            .expect("RFC 3339 expiry")
            .timestamp();
    let message = format!(
        "{}|{}|{epoch}|{expires}",
        request["id"].as_str().expect("id"),
        request["task_id"].as_str().expect("task_id")
    );
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(&key));
    } else {
        block[..key.len()].copy_from_slice(&key);
    }
    let pad = |byte: u8| block.iter().map(|k| k ^ byte).collect::<Vec<u8>>();
    let inner = Sha256::new()
        .chain_update(pad(0x36))
        .chain_update(message.as_bytes())
        .finalize();
    let mac = Sha256::new()
        .chain_update(pad(0x5c))
        .chain_update(inner)
        .finalize();
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let (mut buffer, mut bits) = (0u32, 0u32);
    for byte in mac {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    out.truncate(26);
    out
}

/// The bound plugin, confined by the default plugin sandbox, raises the
/// approval digest and submits a human's answer through its own callbacks,
/// and its child still cannot read the channel store. An unbound plugin and
/// an agent are refused with the existing codes and apply nothing.
#[test]
fn a_confined_bound_plugin_runs_the_digest_and_submission_end_to_end() {
    let probe = orbit_exec::probe_landlock();
    if !probe.available {
        // Reported as the orbit-exec Landlock suite does, never as a pass of
        // the native boundary.
        #[allow(clippy::print_stdout)]
        {
            println!("skipping the native Landlock run: {}", probe.detail);
        }
        return;
    }
    let host = Host::new();
    host.add_plugin(CHANNEL);
    host.add_plugin("bystander");
    host.ok(&[
        "decision",
        "channel",
        "register",
        CHANNEL,
        "--human",
        "Alex Rivera",
        "--plugin",
        CHANNEL,
        "--allow-sender",
        SENDER,
    ]);
    let added: Value = serde_json::from_slice(
        &host
            .ok(&[
                "task",
                "add",
                "--title",
                "Proposed work",
                "--description",
                "Held for a human.",
                "--status",
                "proposed",
                "--complexity",
                "low",
                "--acceptance-criteria",
                "[\"done\"]",
                "--json",
            ])
            .stdout,
    )
    .expect("task add JSON");
    let task = added["id"].as_str().expect("task id").to_string();

    for namespace in [CHANNEL, "bystander"] {
        assert_eq!(
            host.callback(namespace, "probe").status,
            "denied",
            "{namespace}: the confined child neither reads nor lists the channel store"
        );
    }

    let digest = host.callback(CHANNEL, "digest");
    assert_eq!(
        digest.status, "0",
        "digest: {}{}",
        digest.stdout, digest.stderr
    );
    let digest: Value = serde_json::from_str(&digest.stdout).expect("digest JSON");
    let request = digest["raised"]
        .as_array()
        .and_then(|raised| {
            raised
                .iter()
                .find(|request| request["task_id"] == task.as_str())
        })
        .unwrap_or_else(|| panic!("the digest raised the task's approval request: {digest}"))
        .clone();

    let store: Value = serde_json::from_slice(
        &std::fs::read(
            host.home
                .join(".orbit/state/decision-channels/channels.json"),
        )
        .expect("channel store"),
    )
    .expect("channel store JSON");
    let registration = &store["channels"][CHANNEL];
    let token = channel_token(
        registration["key_hex"].as_str().expect("key"),
        registration["epoch"].as_u64().expect("epoch"),
        &request,
    );
    let submission = json!({
        "channel": CHANNEL,
        "id": request["id"],
        "answer": "reject",
        "token": token,
        "note": "Not this quarter.",
        "sender": SENDER,
        "auth_results": "mx.example.com; dkim=pass header.d=example.com; spf=pass",
    });
    for namespace in [CHANNEL, "bystander"] {
        std::fs::write(
            host.state(namespace).join("submit.json"),
            submission.to_string(),
        )
        .expect("submission input");
    }

    // A plugin the channel is not bound to: refused on both callbacks.
    for verb in ["digest", "submit"] {
        let refused = host.callback("bystander", verb);
        assert_ne!(refused.status, "0", "bystander {verb} must be refused");
        assert!(
            refused
                .stderr
                .contains("\"code\":\"channel_not_registered\""),
            "bystander {verb}: {}{}",
            refused.stdout,
            refused.stderr
        );
    }
    // An agent presenting the same valid token outside any callback session.
    let agent = host.run(
        &[
            "tool",
            "run",
            "orbit.decision.submit",
            "--input",
            &submission.to_string(),
        ],
        &[("ORBIT_AGENT_NAME", "claude")],
    );
    assert!(!agent.status.success(), "the agent must be refused");
    assert!(
        text(&agent).contains("\"code\":\"capability_denied\""),
        "{}",
        text(&agent)
    );
    assert_eq!(
        host.task_status(&task),
        "proposed",
        "nothing was applied yet"
    );

    let submitted = host.callback(CHANNEL, "submit");
    assert_eq!(
        submitted.status, "0",
        "submit: {}{}",
        submitted.stdout, submitted.stderr
    );
    let answered: Value = serde_json::from_str(&submitted.stdout).expect("submit JSON");
    assert_eq!(answered["state"], "answered", "{answered}");
    assert_eq!(host.task_status(&task), "rejected");

    let replayed = host.callback(CHANNEL, "submit");
    assert!(
        replayed.stderr.contains("\"code\":\"token_consumed\""),
        "the token is single-use across the route: {}{}",
        replayed.stdout,
        replayed.stderr
    );
}

/// A task-form plugin request retires an already-open generic approval before
/// the callback returns, so a previously issued approval cannot still be
/// answered alongside the plugin's decision.
#[test]
fn a_plugin_request_retires_an_open_task_approval() {
    let probe = orbit_exec::probe_landlock();
    if !probe.available {
        #[allow(clippy::print_stdout)]
        {
            println!("skipping the native Landlock run: {}", probe.detail);
        }
        return;
    }
    let host = Host::new();
    let source = host.root.join("sources/requester/.orbit-plugin");
    std::fs::create_dir_all(source.join("bin")).expect("plugin source");
    let backend = source.join("bin/backend.sh");
    std::fs::write(
        &backend,
        r#"#!/bin/sh
cat >/dev/null
st="$ORBIT_PLUGIN_STATE"
orbit tool run orbit.decision.plugin_raise --input-file "$st/raise.json" >"$st/out" 2>"$st/err"
printf '%s\n' "$?" >"$st/status"
printf '{"ok":true,"output":{}}\n'
"#,
    )
    .expect("plugin backend");
    std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755))
        .expect("chmod backend");
    std::fs::write(
        source.join("plugin.yaml"),
        "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: requester\n  version: 0.1.0\n  description: Plugin decision retirement fixture.\nspec:\n  permissions:\n    orbit_tools: [orbit.decision.plugin_raise]\n    fs:\n      read: [\"{{plugin_state}}\"]\n      write: [\"{{plugin_state}}\"]\n  backend:\n    type: exec\n    command: bin/backend.sh\n  tools:\n    - name: ask\n      description: Raise a fixture decision.\n      execution_kind: mutating\n      mcp_scope: none\n    - name: apply\n      description: Fixture apply tool.\n      execution_kind: mutating\n      agent_callable: false\n      mcp_scope: none\n",
    )
    .expect("plugin manifest");
    let source_arg = source.to_str().expect("utf8 plugin source");
    host.ok(&[
        "plugin",
        "add",
        source_arg,
        "--enable",
        "--grant",
        "fs,orbit_tools",
    ]);

    let added: Value = serde_json::from_slice(
        &host
            .ok(&[
                "task",
                "add",
                "--title",
                "Proposed result",
                "--description",
                "A plugin decision competes with the approval.",
                "--status",
                "proposed",
                "--complexity",
                "low",
                "--acceptance-criteria",
                "[\"result is ready\"]",
                "--json",
            ])
            .stdout,
    )
    .expect("task JSON");
    let task_id = added["id"].as_str().expect("task id").to_string();
    let approval_id = {
        let _authority = test_env::unset(test_env::INHERITED_AUTHORITY_ENV.iter().copied());
        let workspace_root = host.ok(&["workspace", "show", "--format", "json"]).stdout;
        let workspace: Value = serde_json::from_slice(&workspace_root).expect("workspace JSON");
        let runtime = OrbitRuntime::from_roots(
            &host.home.join(".orbit"),
            Path::new(workspace["orbit_root"].as_str().expect("orbit root")),
        )
        .expect("runtime")
        .with_actor(ActorIdentity::human("fixture-operator"));
        let task = runtime.get_task(&task_id).expect("proposed task");
        let request = DecisionRequest::new(
            task.id.clone(),
            DecisionGate::ProposedApproval,
            vec![
                DecisionAnswer::plain("approve", "Approve", "Move to backlog."),
                DecisionAnswer::plain("reject", "Reject", "Decline the task."),
            ],
            DecisionBinding::for_task(&task, None),
            "fixture".into(),
            chrono::Utc::now(),
        );
        let id = request.id.clone();
        EngineHost::new(&runtime)
            .apply_task_automation_update(
                &task_id,
                TaskAutomationUpdate {
                    append_history: vec![request.history_entry("fixture")],
                    ..Default::default()
                },
            )
            .expect("seed open approval");
        id
    };
    let state = host.state("requester");
    std::fs::create_dir_all(&state).expect("plugin state");
    std::fs::write(
        state.join("raise.json"),
        json!({
            "task_id": task_id,
            "subject": "result-1",
            "summary": "Accept the plugin result?",
            "answers": [
                {"id": "accept", "label": "Accept", "consequence": "Record acceptance."},
                {"id": "reject", "label": "Reject", "consequence": "Record rejection."}
            ],
            "recommended": "accept",
            "apply": {
                "tool": "requester.apply",
                "revision": "rev-1",
                "input": {"result": "result-1"}
            }
        })
        .to_string(),
    )
    .expect("plugin input");
    let callback = host.run(
        &["tool", "run", "requester.ask", "--input", "{}"],
        &[("ORBIT_OPERATOR", "1")],
    );
    assert!(callback.status.success(), "{}", text(&callback));
    assert_eq!(
        std::fs::read_to_string(state.join("status"))
            .expect("callback status")
            .trim(),
        "0",
        "{}{}",
        std::fs::read_to_string(state.join("out")).unwrap_or_default(),
        std::fs::read_to_string(state.join("err")).unwrap_or_default()
    );

    let retired: Value = serde_json::from_slice(
        &host
            .ok(&["decision", "show", &approval_id, "--json"])
            .stdout,
    )
    .expect("retired approval JSON");
    assert_eq!(retired["state"], "superseded", "{retired}");
    let raised: Value =
        serde_json::from_slice(&std::fs::read(state.join("out")).expect("plugin callback output"))
            .expect("plugin decision JSON");
    assert_eq!(raised["state"], "open", "{raised}");
    assert_eq!(raised["gate"], "plugin_request", "{raised}");

    let answer = host.run(
        &["decision", "answer", &approval_id, "approve", "--json"],
        &[("ORBIT_OPERATOR", "1")],
    );
    assert!(
        !answer.status.success(),
        "the retired approval was answered"
    );
    let refusal: Value = serde_json::from_slice(&answer.stderr).expect("refusal JSON");
    assert_eq!(refusal["code"], "request_superseded", "{refusal}");
    assert_eq!(host.task_status(&task_id), "proposed");
}
