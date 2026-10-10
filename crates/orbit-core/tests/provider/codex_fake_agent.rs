#![cfg(unix)]
//! The PATH a Codex agent's shell commands resolve tools through [ORB-15204].
//!
//! A Mac reviewer ran `/usr/bin/python3` (3.9) while host validation ran
//! Homebrew's through `workflow.validation_env.path`, so the two disagreed on
//! the same base. Orbit puts the configured entries ahead of the agent's PATH,
//! but Codex runs every tool command as `<user shell> -lc <command>`: the login
//! profiles run again, and on macOS `/etc/zprofile`'s `path_helper` moves the
//! inherited entries behind `/usr/bin`. Codex runs `<user shell> -c` instead
//! when started with `--config allow_login_shell=false`.
//!
//! Contract verified against codex-cli 0.162.1 on macOS, driven against a
//! local Responses API stub: with PATH `/opt/homebrew/bin:/usr/bin:…` its
//! `exec_command` resolved `/usr/bin/python3` by default and
//! `/opt/homebrew/bin/python3` with `allow_login_shell=false`, at the checkout
//! and in a scratch directory alike.
//!
//! The fake below keeps that shell selection. Its login profile puts another
//! `python3` first, as `path_helper` does, under a throwaway `HOME` so no real
//! profile is read.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use orbit_core::OrbitRuntime;
use orbit_engine::{DispatchOutcome, V2AuditWriter, V2DispatchInput, dispatch_v2_activity};
use orbit_types::resource::ExecutorResource;
use orbit_types::workflow::ExecutorDef;
use orbit_types::workflow::activity_job::{ActivityV2Spec, AgentLoopSpec, OnDenial, Provider};

const VALIDATION_PYTHON: &str = "Python 3.99.0 (validation_env)";
const PROFILE_PYTHON: &str = "Python 3.9.6 (login profile)";

struct Harness {
    dir: tempfile::TempDir,
    runtime: OrbitRuntime,
}

impl Harness {
    /// A runtime whose workspace `config.toml` is `config` (`{DIR}` names the
    /// harness directory), with the shipped Codex executor pointed at the fake.
    fn new(config: &str) -> Self {
        let dir = tempfile::tempdir().expect("harness tempdir");
        let root = dir.path();
        fake_python(&root.join("validation-bin"), VALIDATION_PYTHON);
        fake_python(&root.join("profile-bin"), PROFILE_PYTHON);
        let home = root.join("home");
        std::fs::create_dir_all(&home).expect("create shell home");
        let profile = format!(
            "export PATH='{}':\"$PATH\"\n",
            root.join("profile-bin").display()
        );
        for name in [".zprofile", ".bash_profile", ".profile"] {
            std::fs::write(home.join(name), &profile).expect("write login profile");
        }
        let program = fake_codex(root);

        let global = root.join("global");
        let workspace = root.join("workspace/.orbit");
        std::fs::create_dir_all(&global).expect("create global root");
        std::fs::create_dir_all(&workspace).expect("create workspace root");
        std::fs::write(
            workspace.join("config.toml"),
            config.replace("{DIR}", &root.display().to_string()),
        )
        .expect("write workspace config");
        let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");
        seed_codex_executor(&runtime, &program);
        Self { dir, runtime }
    }

    fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("argv.txt"))
            .expect("fake codex recorded argv")
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// What `python3 --version` printed in the agent's tool shell.
    fn tool_python(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("python3-version.txt"))
            .expect("fake codex ran python3 in its tool shell")
            .trim()
            .to_string()
    }
}

fn fake_python(dir: &Path, version: &str) {
    std::fs::create_dir_all(dir).expect("create interpreter dir");
    let python = dir.join("python3");
    std::fs::write(&python, format!("#!/bin/sh\necho '{version}'\n")).expect("write python3");
    std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755))
        .expect("chmod python3");
}

/// A `codex exec` that runs one tool command the way Codex does: through the
/// user's shell, as a login shell unless `allow_login_shell=false`.
fn fake_codex(dir: &Path) -> PathBuf {
    let program = dir.join("codex");
    let script = format!(
        r#"#!/bin/sh
printf '%s\n' "$@" > '{dir}/argv.txt'
cat > /dev/null
mode=-lc
for arg in "$@"; do
  [ "$arg" = "allow_login_shell=false" ] && mode=-c
done
shell=/bin/zsh
[ -x "$shell" ] || shell=/bin/bash
HOME='{dir}/home' ZDOTDIR='{dir}/home' "$shell" "$mode" 'python3 --version' > '{dir}/python3-version.txt' 2>&1
printf '%s\n' '{{"schemaVersion":1,"status":"success","result":{{"probed":true}},"error":null}}'
"#,
        dir = dir.display(),
    );
    std::fs::write(&program, script).expect("write fake codex");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake codex");
    program
}

fn seed_codex_executor(runtime: &OrbitRuntime, program: &Path) {
    let resource: ExecutorResource =
        serde_yaml::from_str(include_str!("../../assets/executors/codex.yaml"))
            .expect("parse embedded Codex executor");
    let mut def = ExecutorDef::from_resource_spec(
        resource.metadata.name.clone(),
        resource.spec.clone(),
        resource.spec.created_at,
        resource.spec.updated_at,
    );
    def.command = Some(program.to_string_lossy().into_owned());
    // Sandbox compilation has its own coverage; the shell boundary is the
    // subject here, so keep the outcome independent of host SBPL/bwrap.
    def.sandbox = None;
    runtime
        .upsert_executor_def(&def)
        .expect("seed Codex executor");
}

fn dispatch(harness: &Harness) -> DispatchOutcome {
    let audit_dir = tempfile::tempdir().expect("audit tempdir");
    let audit = V2AuditWriter::with_disk_sinks(
        audit_dir.path(),
        Arc::new(orbit_store::Store::open_in_memory().expect("audit store")),
        "ws_test",
        "codex-fake",
        "codex:test".to_string(),
        None,
    )
    .expect("build audit writer");
    let spec = ActivityV2Spec::AgentLoop(AgentLoopSpec {
        tool_disallow_list: None,
        instruction: "Return the requested Orbit response envelope.".to_string(),
        tools: Vec::new(),
        on_denial: OnDenial::Terminate,
        model: None,
        reasoning_effort: None,
        max_iterations: 1,
        backend: None,
        provider: Provider::Codex,
        wall_clock_timeout_seconds: 60,
        require_response_envelope: true,
        require_completion_envelope: true,
        proc_allowed_programs: None,
        proc_disallowed_programs: None,
        trusted_host_execution: false,
    });
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: "codex_fake_agent",
        spec: &spec,
        fs_profile: None,
        input: serde_json::json!({"prompt": "Run python3 --version."}),
        audit,
        run_id: "codex-fake",
        host: Some(&harness.runtime),
    })
    .expect("dispatch Codex CLI backend");
    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);
    outcome
}

/// With `workflow.validation_env.path` set, a command Codex runs resolves the
/// configured interpreter even though a login profile would put another first.
#[test]
fn codex_tool_commands_resolve_python3_through_the_validation_env_prefix() {
    let harness = Harness::new(
        "[workflow.validation_env]\nlogin_shell = false\npath = [\"{DIR}/validation-bin\"]\n",
    );
    dispatch(&harness);

    assert!(
        harness
            .argv()
            .windows(2)
            .any(|pair| pair == ["--config", "allow_login_shell=false"]),
        "Codex must be told not to rerun login profiles: {:?}",
        harness.argv(),
    );
    assert_eq!(
        harness.tool_python(),
        VALIDATION_PYTHON,
        "the agent's tool shell must resolve the python3 validation_env.path puts first",
    );
}

/// Without a configured path Orbit leaves Codex's login shell alone, so
/// toolchains that only a login profile adds stay available to the agent.
#[test]
fn codex_keeps_its_login_shell_when_no_validation_path_is_configured() {
    let harness = Harness::new("[workflow.validation_env]\nlogin_shell = false\n");
    dispatch(&harness);

    assert!(
        !harness
            .argv()
            .iter()
            .any(|arg| arg.starts_with("allow_login_shell")),
        "{:?}",
        harness.argv(),
    );
    assert_eq!(harness.tool_python(), PROFILE_PYTHON);
}
