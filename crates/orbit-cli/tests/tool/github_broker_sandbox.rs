//! The read-only `github.*` tools from inside a real Bubblewrap agent sandbox
//! (design `docs/design/plugins/2_agent_call_broker.md` §3, §4.4).
//!
//! The sandbox is compiled the way a managed launch compiles it, so it masks
//! `~/.config/gh` and the plugin trees, and the agent's environment comes from
//! the host's execution-env policy. A stand-in `gh` that needs the host's
//! config fails inside it, and the broker runs the call on the host instead.
#![cfg(target_os = "linux")]
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use orbit_core::OrbitRuntime;
use orbit_core::runtime::plugin::sandbox_mask::prepare_plugin_mask;
use orbit_engine::{PluginBrokerRun, RuntimeHost};
use orbit_exec::{LinuxBwrapMask, LinuxBwrapSpawnRequest};
use orbit_tools::plugin::BrokeredCaller;
use orbit_types::policy::ResolvedFsProfile;
use serde_json::Value;

const CHILD: &str = "ORBIT_GITHUB_BROKER_FIXTURE";
/// A host token the agent's environment must not carry.
const HOST_TOKEN: &str = "ghp_host_token_never_in_the_sandbox";

#[test]
fn github_reads_from_a_confined_worker_run_on_the_host() {
    let probe = orbit_exec::probe_bwrap();
    if !probe.available {
        let _ = writeln!(
            std::io::stderr(),
            "skipped github broker sandbox integration: {}",
            probe.detail
        );
        return;
    }
    let root = tempfile::Builder::new()
        .prefix("ogb")
        .tempdir_in("/var/tmp")
        .expect("short sandbox-visible fixture root");
    let bin = root.path().join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let gh = bin.join("gh");
    fs::write(&gh, GH_STUB).expect("gh stub");
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).expect("executable");
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .expect("PATH");
    let mut child = Command::new(std::env::current_exe().expect("test binary"));
    orbit_common::test_env::clear_inherited_authority(|name| {
        child.env_remove(name);
    });
    let output = child
        .env(CHILD, root.path())
        .env("HOME", root.path().join("home"))
        .env("USERPROFILE", root.path().join("home"))
        .env("PATH", path)
        .env("GH_TOKEN", HOST_TOKEN)
        .env("GITHUB_TOKEN", HOST_TOKEN)
        .args([
            "--ignored",
            "--exact",
            "github_broker_sandbox::sandbox_fixture",
            "--nocapture",
        ])
        .output()
        .expect("isolated fixture");
    orbit_common::test_env::assert_child_test_passed(
        "github_broker_sandbox::sandbox_fixture",
        output.status,
        &output.stdout,
        &output.stderr,
    );
}

fn cli(work: &Path, args: &[&str]) -> std::process::Output {
    let output = Command::new(env!("CARGO_BIN_EXE_orbit"))
        .current_dir(work)
        .args(args)
        .output()
        .expect("fixture CLI");
    assert!(
        output.status.success(),
        "orbit {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
#[ignore = "isolated child of the real-bwrap integration test"]
fn sandbox_fixture() {
    let Some(root) = std::env::var_os(CHILD) else {
        return;
    };
    let root = Path::new(&root);
    let home = root.join("home");
    let work = root.join("work");
    let gh_config = home.join(".config/gh");
    fs::create_dir_all(&gh_config).expect("gh config dir");
    fs::write(gh_config.join("hosts.yml"), "github.com: {}\n").expect("host gh config");
    fs::create_dir_all(&work).expect("work");
    let git = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&work)
        .status()
        .expect("git init");
    assert!(git.success());
    cli(
        &work,
        &[
            "init",
            "--non-interactive",
            "--machine-name",
            "github-broker",
            "--task-prefix",
            "TST",
        ],
    );
    cli(&work, &["workspace", "init", "--name", "github-broker"]);

    // What an operator's shell gets from the same tool, unsandboxed.
    let host: Value = serde_json::from_slice(
        &cli(&work, &["tool", "run", "github.run.list", "--input", "{}"]).stdout,
    )
    .expect("host output");
    assert_eq!(host["count"], 1, "{host}");

    let global = home.join(".orbit");
    let runtime = OrbitRuntime::from_roots(&global, &work.join(".orbit")).expect("host runtime");
    let profile = ResolvedFsProfile {
        name: "github_probe".to_string(),
        read: vec!["/**".to_string()],
        modify: vec![format!("{}/**", root.display())],
    };
    let run = PluginBrokerRun {
        run_id: "github-broker-run".to_string(),
        job_run_id: Some("github-broker-run".to_string()),
        task_id: Some("TST-GH".to_string()),
        activity_name: "github_probe".to_string(),
        agent_name: Some("codex".to_string()),
        model_name: None,
        workspace: Some(runtime.workspace_id().expect("workspace identity")),
        allowed_tools: vec!["github.run.list".to_string()],
        tool_deny_policy: None,
        caller: BrokeredCaller {
            worktree: work.clone(),
            fs_profile: profile.clone(),
            proc_allowed_programs: Vec::new(),
            proc_disallowed_programs: None,
        },
    };
    let prepared = prepare_plugin_mask(&global).expect("plugin mask");
    let mask = LinuxBwrapMask {
        sentinel: prepared.sentinel,
        targets: prepared.trees,
    };
    let provider = root.join("provider.py");
    fs::write(&provider, PROVIDER).expect("sandbox provider");
    let plan = orbit_exec::compile_linux_bwrap_argv_with_authority(
        &profile,
        "/usr/bin/python3",
        &[
            provider.display().to_string(),
            env!("CARGO_BIN_EXE_orbit").to_string(),
        ],
        Some(&work),
        false,
        Vec::new(),
        Some(&mask),
    )
    .expect("compile the agent sandbox");
    let broker = runtime
        .start_plugin_broker(&run)
        .expect("start broker")
        .expect("Unix broker");
    let mut env = runtime.agent_subprocess_environment(&[]);
    env.push((
        "ORBIT_PLUGIN_BROKER".to_string(),
        broker.socket_path().display().to_string(),
    ));
    let child = orbit_exec::spawn_under_linux_bwrap(LinuxBwrapSpawnRequest {
        plan: &plan,
        env: &env,
        cwd: Some(&work),
        stdin: Stdio::null(),
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
    })
    .expect("spawn real sandbox");
    broker
        .bind_sandbox(child.id())
        .expect("authenticate sandbox namespace");
    // supervise_child bounds the test and tears down the provider's descendants.
    let output = orbit_exec::supervise_child(child, Some(60_000), None)
        .expect("supervise sandbox")
        .result;
    drop(broker);
    assert!(
        output.success,
        "sandbox provider: {}\n{}",
        output.stdout, output.stderr
    );
    let brokered: Value = serde_json::from_str(&output.stdout).expect("provider report");

    assert_eq!(
        brokered["gh_config_visible"], false,
        "~/.config/gh stays masked: {brokered}"
    );
    assert_eq!(
        brokered["tokens"],
        serde_json::json!([]),
        "no GitHub token reaches the agent: {brokered}"
    );
    assert_ne!(
        brokered["direct_gh"]["code"], 0,
        "gh in the sandbox has no credentials: {brokered}"
    );
    assert_eq!(brokered["tool"]["code"], 0, "{brokered}");
    assert_eq!(
        brokered["tool"]["output"], host,
        "the brokered call returns what the host call does"
    );

    let rows = runtime
        .list_audit_events(None, Some("github.run.list".to_string()), None, None, 10)
        .expect("audit events");
    assert_eq!(
        rows.len(),
        2,
        "the host call and the brokered call; the nested orbit writes none: {rows:?}"
    );
    let brokered_rows = rows.iter().filter(|row| row.brokered).collect::<Vec<_>>();
    assert_eq!(brokered_rows.len(), 1, "{rows:?}");
    assert_eq!(brokered_rows[0].task_id.as_deref(), Some("TST-GH"));
}

/// Needs the host account's config, like the real `gh`: without
/// `~/.config/gh/hosts.yml` it prints the login prompt.
const GH_STUB: &str = r#"#!/bin/sh
[ -f "$HOME/.config/gh/hosts.yml" ] || { echo 'To get started with GitHub CLI, please run:  gh auth login' >&2; exit 4; }
printf '[{"databaseId":7,"number":3,"workflowName":"CI","displayTitle":"fixture","status":"completed","conclusion":"success","event":"push","headBranch":"main","headSha":"abc123","url":"https://example.invalid/run/7"}]\n'
"#;

const PROVIDER: &str = r#"import json, os, subprocess, sys
orbit = sys.argv[1]
def run(argv):
    result = subprocess.run(argv, capture_output=True, text=True, timeout=30)
    report = {'code': result.returncode, 'stderr': result.stderr}
    if result.returncode == 0:
        report['output'] = json.loads(result.stdout)
    return report
print(json.dumps({
    'gh_config_visible': os.path.exists(os.path.expanduser('~/.config/gh/hosts.yml')),
    'tokens': sorted(name for name in ('GH_TOKEN', 'GITHUB_TOKEN') if name in os.environ),
    'direct_gh': run(['gh', 'run', 'list']),
    'tool': run([orbit, 'tool', 'run', 'github.run.list', '--input', '{}']),
}))
"#;
