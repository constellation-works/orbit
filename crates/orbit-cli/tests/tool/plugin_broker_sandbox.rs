//! Plugin authority at the installed-plugin boundary: every case installs a
//! plugin with `orbit plugin add`, then calls it through the built `orbit`
//! binary (design `docs/design/plugins/1_scope.md` §4.1, §4.3;
//! `2_agent_call_broker.md` §6).
//!
//! On Linux the first case also runs the real CLI and MCP transports inside
//! Bubblewrap, served by the host broker.
#![cfg(unix)]
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

use crate::fixture_crew;

#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};

#[cfg(target_os = "linux")]
use orbit_core::OrbitRuntime;
#[cfg(target_os = "linux")]
use orbit_engine::{PluginBrokerRun, RuntimeHost};
#[cfg(target_os = "linux")]
use orbit_tools::plugin::BrokeredCaller;
#[cfg(target_os = "linux")]
use orbit_types::policy::ResolvedFsProfile;
#[cfg(target_os = "linux")]
use serde_json::Value;

#[cfg(target_os = "linux")]
const CHILD: &str = "ORBIT_SANDBOX_BROKER_FIXTURE";

#[cfg(target_os = "linux")]
#[test]
fn cli_and_mcp_reach_host_backends_with_authoritative_identity() {
    let probe = orbit_exec::probe_bwrap();
    if !probe.available {
        let _ = writeln!(
            std::io::stderr(),
            "skipped broker sandbox integration: {}",
            probe.detail
        );
        return;
    }
    // Runtime bootstrap and CLI mutations are isolated from the test runner's
    // environment and managed workspace authority.
    let root = tempfile::Builder::new()
        .prefix("obe")
        .tempdir_in("/var/tmp")
        .expect("short sandbox-visible fixture root");
    let mut child = Command::new(std::env::current_exe().expect("test binary"));
    orbit_common::test_env::clear_inherited_authority(|name| {
        child.env_remove(name);
    });
    let output = child
        .env(CHILD, root.path())
        .env("HOME", root.path().join("home"))
        .env("USERPROFILE", root.path().join("home"))
        .args([
            "--ignored",
            "--exact",
            "plugin_broker_sandbox::sandbox_fixture",
            "--nocapture",
        ])
        .output()
        .expect("isolated fixture");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
#[test]
#[ignore = "isolated child of the real-bwrap integration test"]
fn sandbox_fixture() {
    let Some(root) = std::env::var_os(CHILD) else {
        return;
    };
    let root = Path::new(&root);
    let home = root.join("home");
    let work = root.join("work");
    fs::create_dir_all(&home).expect("home");
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
            "broker-fixture",
            "--task-prefix",
            "TST",
        ],
    );
    cli(&work, &["workspace", "init", "--name", "broker-fixture"]);
    let routing: Value =
        serde_json::from_slice(&cli(&work, &["workspace", "show", "--format", "json"]).stdout)
            .expect("routing");
    assert_eq!(
        routing["checkout"]["repo_root"],
        work.to_str().expect("work")
    );
    for (name, kind) in [("execfixture", "exec"), ("mcpfixture", "mcp")] {
        let source = root.join(name).join(".orbit-plugin");
        fs::create_dir_all(source.join("bin")).expect("plugin dirs");
        let backend = source.join("bin/backend");
        fs::write(
            &backend,
            if kind == "exec" {
                EXEC_BACKEND
            } else {
                MCP_BACKEND
            },
        )
        .expect("backend");
        fs::set_permissions(&backend, fs::Permissions::from_mode(0o755)).expect("executable");
        fs::write(source.join("plugin.yaml"), format!(
            "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {name}\n  version: 1.0.0\n  description: Broker integration fixture.\nspec:\n  backend:\n    type: {kind}\n    command: bin/backend\n  tools:\n    - name: echo\n      description: Echo authenticated context.\n      execution_kind: read_only\n      mcp_scope: workspace\n      input_schema:\n        type: object\n"
        )).expect("manifest");
        cli(
            &work,
            &[
                "plugin",
                "add",
                source.to_str().expect("source"),
                "--enable",
            ],
        );
    }
    let global = home.join(".orbit");
    let runtime = OrbitRuntime::from_roots(&global, &work.join(".orbit")).expect("host runtime");
    let run = PluginBrokerRun {
        run_id: "broker-real-run".to_string(),
        job_run_id: Some("broker-real-run".to_string()),
        task_id: Some("TST-REAL".to_string()),
        activity_name: "sandbox_probe".to_string(),
        agent_name: Some("codex".to_string()),
        model_name: None,
        workspace: Some(runtime.workspace_id().expect("workspace identity")),
        allowed_tools: vec![
            "execfixture.echo".to_string(),
            "mcpfixture.echo".to_string(),
        ],
        tool_deny_policy: None,
        caller: BrokeredCaller {
            worktree: work.clone(),
            fs_profile: ResolvedFsProfile {
                name: "sandbox_probe".to_string(),
                read: vec!["/**".to_string()],
                modify: vec![format!("{}/**", root.display())],
            },
            proc_allowed_programs: Vec::new(),
            proc_disallowed_programs: None,
        },
    };
    let broker = runtime
        .start_plugin_broker(&run)
        .expect("start broker")
        .expect("Unix broker");
    let socket = broker.socket_path().to_path_buf();
    let provider = root.join("provider.py");
    fs::write(&provider, PROVIDER).expect("sandbox provider");
    let child = Command::new("/usr/bin/bwrap")
        .args([
            "--unshare-user",
            "--unshare-pid",
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--bind",
        ])
        .arg(root)
        .arg(root)
        .args(["--chdir"])
        .arg(&work)
        .args(["--", "/usr/bin/python3"])
        .arg(&provider)
        .arg(env!("CARGO_BIN_EXE_orbit"))
        .env("ORBIT_PLUGIN_BROKER", &socket)
        .env("ORBIT_RUN_ID", "spoofed-run")
        .env("ORBIT_TASK_ID", "TST-SPOOFED")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("spawn real sandbox");
    broker
        .bind_sandbox(child.id())
        .expect("authenticate sandbox namespace");
    // supervise_child bounds the test and tears down the provider's descendants.
    let output = orbit_exec::supervise_child(child, Some(60_000), None)
        .expect("supervise sandbox")
        .result;
    assert!(
        output.success,
        "sandbox provider: {}\n{}",
        output.stdout, output.stderr
    );
    let rows: Vec<Value> = serde_json::from_str(&output.stdout).expect("provider results");
    assert_eq!(rows.len(), 4);
    for row in &rows {
        assert_eq!(
            row["parent"].as_u64(),
            Some(u64::from(std::process::id())),
            "backend is a child of the host worker, not of a nested CLI or sandbox"
        );
        assert_eq!(row["context"]["task_id"], "TST-REAL");
        assert_eq!(row["context"]["job_run_id"], "broker-real-run");
    }
    assert_eq!(
        rows[1]["pid"], rows[3]["pid"],
        "CLI and MCP clients in one invocation reuse its backend"
    );
    let pid = rows[1]["pid"].as_u64().expect("MCP pid") as i32;
    drop(broker);
    assert!(!socket.exists());
    // SAFETY: signal zero checks existence only. Teardown waits for reaping.
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "the run's MCP session was reclaimed"
    );
}

#[cfg(target_os = "linux")]
const EXEC_BACKEND: &str = r#"#!/usr/bin/python3
import json, os, sys
request = json.load(sys.stdin)
print(json.dumps({'ok': True, 'output': {'pid': os.getpid(), 'parent': os.getppid(), 'context': request['context']}}))
"#;

#[cfg(target_os = "linux")]
const MCP_BACKEND: &str = r#"#!/usr/bin/python3
import json, os, sys
for line in sys.stdin:
    request = json.loads(line)
    method = request.get('method')
    if method == 'initialize':
        result = {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'fixture', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': [{'name': 'echo', 'inputSchema': {'type': 'object'}}]}
    elif method == 'tools/call':
        result = {'content': [], 'structuredContent': {'pid': os.getpid(), 'parent': os.getppid(), 'context': request['params']['_meta']['orbit']}}
    else:
        continue
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
"#;

#[cfg(target_os = "linux")]
const PROVIDER: &str = r#"import json, os, subprocess, sys
orbit = sys.argv[1]
rows = []
for ns in ['execfixture', 'mcpfixture']:
    result = subprocess.run([orbit, 'tool', 'run', ns + '.echo', '--input', '{}'], capture_output=True, text=True, timeout=20)
    assert result.returncode == 0, result.stderr
    rows.append(json.loads(result.stdout))
server = subprocess.Popen([orbit, 'mcp', 'serve'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=sys.stderr, text=True)
def send(message):
    server.stdin.write(json.dumps(message) + '\n')
    server.stdin.flush()
def request(id, method, params):
    send({'jsonrpc': '2.0', 'id': id, 'method': method, 'params': params})
    while True:
        line = server.stdout.readline()
        assert line, 'MCP server exited before replying'
        response = json.loads(line)
        if response.get('id') == id:
            assert 'error' not in response, response
            return response['result']
try:
    request(1, 'initialize', {'protocolVersion': '2025-06-18', 'capabilities': {}, 'clientInfo': {'name': 'probe', 'version': '1'}, '_meta': {'orbit': {'workspace': os.getcwd()}}})
    send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
    for id, ns in enumerate(['execfixture', 'mcpfixture'], 2):
        result = request(id, 'tools/call', {'name': ns + '_echo', 'arguments': {}})
        assert not result.get('isError'), result
        output = result.get('structuredContent')
        if output is None:
            output = json.loads(result['content'][0]['text'])
        rows.append(output)
finally:
    server.terminate()
    server.wait(timeout=5)
print(json.dumps(rows))
"#;

mod authority {
    //! Grants, the authorization witness, deterministic-step program bounds, the
    //! plugin-state masks and the operator floor, each exercised through a plugin
    //! installed with `orbit plugin add` and called through the built binary.
    //!
    //! Most fixtures run their backend with `backend.sandbox: none`, consented
    //! with `--grant unsandboxed`. Every rule they guard is enforced before the
    //! backend is spawned, and an unconfined backend lets each case's positive
    //! control run on a host that cannot nest a sandbox. Only the state-isolation
    //! case depends on the backend sandbox itself.

    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, ChildStdin, Output, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    use assert_cmd::cargo::cargo_bin_cmd;
    use orbit_common::test_env;
    use rusqlite::Connection;
    use serde_json::{Value, json};
    use tempfile::TempDir;

    use super::fixture_crew;

    /// Every child the fixtures start is bounded: a hung command fails its test
    /// instead of stalling the suite.
    const CHILD_TIMEOUT: Duration = Duration::from_secs(120);

    /// One disposable Orbit host: its home, a registered workspace, and the file
    /// every unconfined fixture backend appends a line to when it runs.
    struct Host {
        _temp: TempDir,
        root: PathBuf,
        home: PathBuf,
        work: PathBuf,
    }

    impl Host {
        fn new() -> Self {
            // `sandbox-exec` matches resolved paths, and the default macOS temp
            // directory sits behind the `/var` symlink.
            let temp = tempfile::tempdir_in(test_env::canonical_temp_dir()).expect("tempdir");
            let root = temp.path().to_path_buf();
            let home = root.join("home");
            let work = root.join("work");
            std::fs::create_dir_all(&home).expect("home");
            std::fs::create_dir_all(&work).expect("work");
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
                "plugin-authority",
                "--task-prefix",
                "TST",
            ]);
            host.ok(&["workspace", "init", "--name", "plugin-authority"]);
            host
        }

        fn global(&self) -> PathBuf {
            self.home.join(".orbit")
        }

        /// A child `orbit` pinned to this host with no inherited authority: the
        /// test runner's own run, operator status or broker decides nothing.
        fn orbit(&self, env: &[(&str, &str)]) -> assert_cmd::Command {
            let mut command = cargo_bin_cmd!("orbit");
            test_env::clear_inherited_authority(|name| {
                command.env_remove(name);
            });
            command
                .env_remove("AGENT_RUN_ID")
                .current_dir(&self.work)
                .env("HOME", &self.home)
                .env("USERPROFILE", &self.home)
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

        fn show(&self, namespace: &str) -> Value {
            let output = self.ok(&["plugin", "show", namespace, "--format", "json"]);
            serde_json::from_slice(&output.stdout).expect("plugin show JSON")
        }

        /// `orbit tool run <tool>` as this machine's operator, who may call any
        /// active plugin tool.
        fn call(&self, tool: &str) -> Output {
            self.run(
                &["tool", "run", tool, "--input", "{}"],
                &[("ORBIT_OPERATOR", "1")],
            )
        }

        fn call_ok(&self, tool: &str) {
            let output = self.call(tool);
            assert!(output.status.success(), "{tool}: {}", text(&output));
        }

        /// Call `tool` as the operator and return the refusal, asserting the
        /// backend never ran.
        fn refused(&self, tool: &str) -> String {
            let before = self.calls();
            let output = self.call(tool);
            let printed = text(&output);
            assert!(
                !output.status.success(),
                "{tool} must be refused: {printed}"
            );
            assert_eq!(
                self.calls(),
                before,
                "{tool} is refused before its backend runs: {printed}"
            );
            printed
        }

        /// The tools whose backend actually ran, in call order.
        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.root.join("calls"))
                .unwrap_or_default()
                .lines()
                .map(ToString::to_string)
                .collect()
        }

        fn db(&self) -> Connection {
            Connection::open(self.global().join("orbit.db")).expect("open the host store")
        }

        /// Overwrite one `plugins` row the way any process that can write
        /// `orbit.db` — a plugin backend among them — could.
        fn write_row(&self, namespace: &str, enabled: bool, grants: &[&str]) {
            let changed = self
                .db()
                .execute(
                    "UPDATE plugins SET enabled = ?1, grants_json = ?2 WHERE name = ?3",
                    rusqlite::params![enabled, json!(grants).to_string(), namespace],
                )
                .expect("write the plugins row");
            assert_eq!(changed, 1, "the {namespace} row exists");
        }

        fn witness(&self, namespace: &str) -> PathBuf {
            self.global()
                .join("plugins/.grants")
                .join(format!("{namespace}.json"))
        }

        fn installed_manifest(&self, namespace: &str, version: &str) -> PathBuf {
            self.global()
                .join("plugins")
                .join(namespace)
                .join(version)
                .join("plugin.yaml")
        }

        /// A plugin source outside the checkout (a source inside a repository is
        /// refused) whose tools are `probe` (read-only) and `mutate`.
        fn write_plugin(&self, namespace: &str, version: &str, plugin: &Plugin<'_>) -> PathBuf {
            let source = self.root.join("sources").join(namespace).join(version);
            let tree = source.join(".orbit-plugin");
            let calls = self.root.join("calls");
            let backend = match plugin.backend {
                Backend::Recording => recording_backend(&calls, None),
                Backend::RunsProgram(program) => recording_backend(&calls, Some(program)),
                Backend::StateProbe { other } => state_probe_backend(&self.global(), other),
            };
            write_executable(&tree.join("bin/backend.sh"), &backend);
            let sandbox = if plugin.unconfined {
                "    sandbox: none\n"
            } else {
                ""
            };
            let definitions = if plugin.job {
                write_job_definitions(&tree, namespace)
            } else {
                String::new()
            };
            std::fs::write(
            tree.join("plugin.yaml"),
            format!(
                "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {namespace}\n  version: \
                 {version}\n  description: Plugin authority fixture.\nspec:\n{extra}  backend:\n    \
                 type: exec\n    command: bin/backend.sh\n{sandbox}  tools:\n    - name: probe\n      \
                 description: Report that the backend ran.\n      execution_kind: read_only\n      \
                 mcp_scope: workspace\n    - name: mutate\n      description: Change something.\n      \
                 execution_kind: mutating\n      mcp_scope: workspace\n{definitions}",
                extra = plugin.extra_spec,
            ),
        )
        .expect("manifest");
            source
        }

        fn add(&self, source: &Path, flags: &[&str]) {
            let mut args = vec!["plugin", "add", source.to_str().expect("utf8 source")];
            args.extend(flags);
            self.ok(&args);
        }
    }

    #[derive(Clone, Copy)]
    enum Backend<'a> {
        /// Appends `$ORBIT_TOOL_NAME` to the host's `calls` file.
        Recording,
        /// Runs the declared program at this path first; the program records
        /// itself, then the backend records the call.
        RunsProgram(&'a str),
        /// Reports what it could reach of its own state, the named plugin's
        /// state and the tree holding both.
        StateProbe { other: &'a str },
    }

    struct Plugin<'a> {
        backend: Backend<'a>,
        unconfined: bool,
        /// Spliced under `spec:` ahead of the backend.
        extra_spec: &'a str,
        /// Ship a deterministic `plugin.tool_call` activity calling `probe`, and
        /// a job running it as its one step.
        job: bool,
    }

    impl<'a> Plugin<'a> {
        fn unconfined() -> Self {
            Self {
                backend: Backend::Recording,
                unconfined: true,
                extra_spec: "",
                job: false,
            }
        }
    }

    fn recording_backend(calls: &Path, program: Option<&str>) -> String {
        let program = program
            .map(|program| format!("'{program}' || exit 3\n"))
            .unwrap_or_default();
        // Shell builtins only: a job worker's PATH is not the test's.
        format!(
            "#!/bin/sh\nwhile IFS= read -r _; do :; done\n{program}printf '%s\\n' \"$ORBIT_TOOL_NAME\" >> \
         '{calls}'\nprintf '{{\"ok\":true,\"output\":{{\"tool\":\"%s\"}}}}\\n' \"$ORBIT_TOOL_NAME\"\n",
            calls = calls.display(),
        )
    }

    /// Writes a marker into its own state and reads it back, then probes the
    /// other plugin's state and `state/plugins/`, which holds both. A boundary
    /// that holds leaves `result` at `ok`; each access it should not allow is
    /// appended.
    fn state_probe_backend(global: &Path, other: &str) -> String {
        let other_state = global.join("state/plugins").join(other);
        format!(
            r#"#!/bin/sh
cat >/dev/null
result=ok
echo "$ORBIT_PLUGIN" > "$ORBIT_PLUGIN_STATE/marker" 2>/dev/null || result="$result,own_write_denied"
[ "$(cat "$ORBIT_PLUGIN_STATE/marker" 2>/dev/null)" = "$ORBIT_PLUGIN" ] || result="$result,own_read_denied"
if cat '{other}/secret' >/dev/null 2>&1; then result="$result,other_read"; fi
if ls '{other}' >/dev/null 2>&1; then result="$result,other_listed"; fi
if ls "$(dirname "$ORBIT_PLUGIN_STATE")" >/dev/null 2>&1; then result="$result,state_tree_listed"; fi
printf '{{"ok":true,"output":{{"result":"%s"}}}}\n' "$result"
"#,
            other = other_state.display(),
        )
    }

    /// The activity and job a plugin ships so a routine can drive its tool, and
    /// the manifest lines that declare them.
    fn write_job_definitions(tree: &Path, namespace: &str) -> String {
        let activities = tree.join("definitions/activities");
        let jobs = tree.join("definitions/jobs");
        std::fs::create_dir_all(&activities).expect("activities");
        std::fs::create_dir_all(&jobs).expect("jobs");
        std::fs::write(
        activities.join("refresh.yaml"),
        format!(
            "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: {namespace}_refresh\nspec:\n  \
             type: deterministic\n  description: Call the plugin tool.\n  input_schema_json:\n    \
             type: object\n  action: plugin.tool_call\n  config:\n    tool: {namespace}.probe\n    \
             input: {{}}\n"
        ),
    )
    .expect("activity");
        std::fs::write(
        jobs.join("pipeline.yaml"),
        format!(
            "schemaVersion: 2\nkind: Job\nmetadata:\n  name: {namespace}_refresh_pipeline\nspec:\n  \
             state: enabled\n  kind: workflow\n  max_active_runs: 1\n  steps:\n    - id: refresh\n      \
             target: activity:{namespace}_refresh\n"
        ),
    )
    .expect("job");
        "  definitions:\n    activities: [definitions/activities/*.yaml]\n    jobs: \
     [definitions/jobs/*.yaml]\n"
            .to_string()
    }

    fn write_executable(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(path, contents).expect("write executable");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    fn text(output: &Output) -> String {
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }

    /// A grant is authority only when `orbit plugin enable` recorded it: a
    /// manifest request nobody granted, a grant written straight into the
    /// `plugins` row, and a recorded grant whose authorization witness is gone
    /// are each refused at call time, before the backend runs [ORB-12778].
    #[test]
    fn a_grant_that_was_not_recorded_is_refused_at_call_time() {
        let host = Host::new();
        let source = host.write_plugin("loose", "1.0.0", &Plugin::unconfined());

        // Enabled, but the `unsandboxed` it requests was never granted.
        host.add(&source, &["--enable"]);
        assert_eq!(host.show("loose")["status"], "inactive");
        let refusal = host.refused("loose.probe");
        assert!(
            refusal.contains("--grant unsandboxed"),
            "the refusal names the missing grant: {refusal}"
        );

        // A store writer grants it in the row; the witness still says nothing
        // was authorized.
        host.write_row("loose", true, &["unsandboxed"]);
        let shown = host.show("loose");
        assert_eq!(shown["status"], "inactive", "{shown}");
        assert!(
            shown["diagnostic"]
                .as_str()
                .is_some_and(|message| message.contains("do not match")),
            "{shown}"
        );
        host.refused("loose.probe");
        let denied: i64 = host
            .db()
            .query_row(
                "SELECT COUNT(*) FROM audit_events WHERE command = 'plugin.load' \
             AND target_id = 'loose' AND status = 'denied' \
             AND arguments_json LIKE '%unsandboxed%'",
                [],
                |row| row.get(0),
            )
            .expect("audit query");
        assert!(denied > 0, "the refused row is in the audit trail");

        // The one command that records authority makes the same set effective.
        host.ok(&["plugin", "enable", "loose", "--grant", "unsandboxed"]);
        assert_eq!(host.show("loose")["status"], "active");
        host.call_ok("loose.probe");
        assert_eq!(host.calls(), ["loose.probe"]);

        // A row whose witness was removed claims grants nobody can vouch for.
        std::fs::remove_file(host.witness("loose")).expect("remove the witness");
        let refusal = host.refused("loose.probe");
        assert!(
            refusal.contains("no authorization record"),
            "the refusal names the missing record: {refusal}"
        );
    }

    /// Grants apply to the manifest they were given for. An on-disk edit to the
    /// installed manifest refuses the plugin until the edit is undone, and an
    /// upgrade that widens a request revokes the witness, so restoring the old
    /// grant set in the row cannot carry it onto the wider manifest.
    #[test]
    fn a_changed_manifest_invalidates_the_witness() {
        let host = Host::new();
        let loopback = Plugin {
            extra_spec: "  permissions:\n    network: loopback\n",
            ..Plugin::unconfined()
        };
        host.add(
            &host.write_plugin("drift", "1.0.0", &loopback),
            &["--enable", "--grant", "network,unsandboxed"],
        );
        host.call_ok("drift.probe");

        let manifest = host.installed_manifest("drift", "1.0.0");
        let original = std::fs::read(&manifest).expect("installed manifest");
        let mut edited = original.clone();
        edited.extend_from_slice(b"# edited after install\n");
        std::fs::write(&manifest, &edited).expect("edit the installed manifest");
        let shown = host.show("drift");
        assert_eq!(shown["status"], "inactive", "{shown}");
        assert!(
            shown["diagnostic"]
                .as_str()
                .is_some_and(|message| message.contains("does not match the stored digest")),
            "{shown}"
        );
        host.refused("drift.probe");
        std::fs::write(&manifest, &original).expect("restore the installed manifest");
        host.call_ok("drift.probe");

        let any = Plugin {
            extra_spec: "  permissions:\n    network: any\n",
            ..Plugin::unconfined()
        };
        host.add(&host.write_plugin("drift", "1.1.0", &any), &[]);
        let shown = host.show("drift");
        assert_eq!(shown["status"], "disabled", "{shown}");
        assert_eq!(shown["granted"], json!([]), "{shown}");

        host.write_row("drift", true, &["network", "unsandboxed"]);
        let shown = host.show("drift");
        assert_eq!(shown["status"], "inactive", "{shown}");
        assert!(
            shown["diagnostic"]
                .as_str()
                .is_some_and(|message| message.contains("do not match")),
            "the replayed grant set must not match the revoked witness: {shown}"
        );
        host.refused("drift.probe");

        host.ok(&[
            "plugin",
            "enable",
            "drift",
            "--grant",
            "network,unsandboxed",
        ]);
        host.call_ok("drift.probe");
        assert_eq!(host.calls(), ["drift.probe", "drift.probe", "drift.probe"]);
    }

    /// A deterministic job step spawns what the operator granted the plugin at
    /// enable and nothing else: a declared program that resolved runs, and one
    /// that did not is refused by name before the backend starts [ORB-13270].
    #[test]
    fn deterministic_steps_stay_within_the_granted_programs() {
        let host = Host::new();
        fixture_crew::configure_sol(&host.global());
        let program = host.root.join("bin/fixture-program");
        write_executable(
            &program,
            &format!(
                "#!/bin/sh\nprintf 'program\\n' >> '{}'\n",
                host.root.join("calls").display()
            ),
        );
        let missing = host.root.join("absent/fixture-program");
        for (namespace, program) in [("graph", &program), ("ghost", &missing)] {
            let program = program.to_str().expect("utf8 program");
            let requires = format!("  requires:\n    programs: [\"{program}\"]\n");
            let plugin = Plugin {
                backend: Backend::RunsProgram(program),
                extra_spec: &requires,
                job: true,
                ..Plugin::unconfined()
            };
            host.add(
                &host.write_plugin(namespace, "1.0.0", &plugin),
                &["--enable", "--grant", "unsandboxed"],
            );
        }

        let ran = host.run(
            &["job", "run", "graph_refresh_pipeline", "--wait", "--json"],
            &[],
        );
        assert!(ran.status.success(), "{}", text(&ran));
        assert_eq!(
            host.calls(),
            ["program", "graph.probe"],
            "the granted program ran inside the step"
        );

        let refused = host.run(
            &["job", "run", "ghost_refresh_pipeline", "--wait", "--json"],
            &[],
        );
        let printed = text(&refused);
        assert!(!refused.status.success(), "{printed}");
        let run_id = serde_json::from_slice::<Value>(&refused.stdout)
            .ok()
            .and_then(|result| result["run_id"].as_str().map(ToString::to_string))
            .unwrap_or_else(|| panic!("the run is reported: {printed}"));
        let shown = text(&host.ok(&["run", "show", &run_id, "--json"]));
        assert!(
            shown.contains(&format!("program '{}'", missing.display()))
                && shown.contains("not granted"),
            "the step refusal names the program: {shown}"
        );
        assert_eq!(
            host.calls(),
            ["program", "graph.probe"],
            "nothing ran for the step whose program was never granted"
        );
    }

    /// Each backend reads and writes its own `{{plugin_state}}` and is refused
    /// another plugin's state and the `state/plugins/` tree holding both, under
    /// the real backend sandbox.
    #[test]
    fn each_plugin_backend_sees_only_its_own_state() {
        let host = Host::new();
        for (namespace, other) in [("alpha", "beta"), ("beta", "alpha")] {
            let plugin = Plugin {
                backend: Backend::StateProbe { other },
                unconfined: false,
                extra_spec: "  permissions:\n    fs:\n      write: [\"{{plugin_state}}\"]\n",
                job: false,
            };
            host.add(
                &host.write_plugin(namespace, "1.0.0", &plugin),
                &["--enable", "--grant", "fs"],
            );
            let state = host.global().join("state/plugins").join(namespace);
            std::fs::create_dir_all(&state).expect("plugin state");
            std::fs::write(state.join("secret"), namespace).expect("secret");
        }
        for namespace in ["alpha", "beta"] {
            let output = host.call(&format!("{namespace}.probe"));
            assert!(output.status.success(), "{namespace}: {}", text(&output));
            let result: Value = serde_json::from_slice(&output.stdout).expect("probe JSON");
            assert_eq!(
                result["result"], "ok",
                "{namespace}: own state read and written, other state and the state tree refused"
            );
            assert_eq!(
                std::fs::read_to_string(
                    host.global()
                        .join("state/plugins")
                        .join(namespace)
                        .join("marker")
                )
                .expect("marker"),
                format!("{namespace}\n")
            );
        }
    }

    /// An agent sandbox hides `state/plugins/` and `state/plugin-secrets/` from
    /// the agent: Bubblewrap binds a sentinel over each tree, and the macOS
    /// profile denies them. Either tree hidden either way is enough for a nested
    /// `orbit` to refuse an in-process plugin call instead of running the
    /// backend without its state or secrets.
    #[test]
    fn a_masked_agent_sandbox_runs_no_plugin_backend_in_process() {
        let host = Host::new();
        host.add(
            &host.write_plugin("vault", "1.0.0", &Plugin::unconfined()),
            &["--enable", "--grant", "unsandboxed"],
        );
        host.call_ok("vault.probe");
        // A permission bit binds no process that runs as root.
        // SAFETY: `geteuid` only reads the calling process's credentials.
        let as_root = unsafe { libc::geteuid() } == 0;
        for tree in ["state/plugins", "state/plugin-secrets"] {
            let tree = host.global().join(tree);
            std::fs::create_dir_all(&tree).expect("tree");
            let sentinel = tree.join(".orbit-brokered");
            std::fs::write(&sentinel, b"masked").expect("lay the Linux mask");
            let refusal = host.refused("vault.probe");
            assert!(
                refusal.contains("plugin_broker_unavailable"),
                "{}: {refusal}",
                tree.display()
            );
            std::fs::remove_file(&sentinel).expect("lift the Linux mask");

            if !as_root {
                std::fs::set_permissions(&tree, std::fs::Permissions::from_mode(0o000))
                    .expect("lay the macOS mask");
                let refusal = host.refused("vault.probe");
                std::fs::set_permissions(&tree, std::fs::Permissions::from_mode(0o700))
                    .expect("lift the macOS mask");
                assert!(
                    refusal.contains("plugin_broker_unavailable"),
                    "{}: {refusal}",
                    tree.display()
                );
            }
        }
        host.call_ok("vault.probe");
        assert_eq!(host.calls(), ["vault.probe", "vault.probe"]);
    }

    /// A `mutating` plugin tool is an operator or sanctioned-run operation. An
    /// agent reaches the plugin's read-only tool but not that verb, through
    /// either CLI spelling or MCP; on MCP not even with `ORBIT_OPERATOR` in the
    /// server's environment, which only `--operator` stands for.
    #[test]
    fn an_agent_caller_cannot_reach_an_operator_only_plugin_verb() {
        let host = Host::new();
        host.add(
            &host.write_plugin("ops", "1.0.0", &Plugin::unconfined()),
            &["--enable", "--grant", "unsandboxed"],
        );

        for agent in [
            ("ORBIT_AGENT_NAME", "codex"),
            ("ORBIT_TASK_ACTOR_KIND", "agent"),
        ] {
            let read = host.run(&["tool", "run", "ops.probe", "--input", "{}"], &[agent]);
            assert!(read.status.success(), "{agent:?}: {}", text(&read));
            for args in [
                &["tool", "run", "ops.mutate", "--input", "{}"][..],
                &["ops", "mutate"][..],
            ] {
                let output = host.run(args, &[agent]);
                let printed = text(&output);
                assert!(!output.status.success(), "{agent:?} {args:?}: {printed}");
                assert!(
                    printed.contains("plugin.tool.mutating"),
                    "{agent:?} {args:?} names the governed operation: {printed}"
                );
            }
        }
        assert_eq!(host.calls(), ["ops.probe", "ops.probe"]);

        {
            let mut agent = McpServer::start(&host, &[], &[("ORBIT_OPERATOR", "1")]);
            assert!(
                !agent.call("ops_probe")["isError"]
                    .as_bool()
                    .unwrap_or(false)
            );
            let refusal = agent.call("ops_mutate");
            assert_eq!(refusal["isError"], true, "{refusal}");
            assert!(
                refusal.to_string().contains("plugin.tool.mutating"),
                "{refusal}"
            );
        }
        assert_eq!(host.calls(), ["ops.probe", "ops.probe", "ops.probe"]);

        // The operator surfaces reach the same verb.
        host.call_ok("ops.mutate");
        {
            let mut operator = McpServer::start(&host, &["--operator"], &[]);
            let result = operator.call("ops_mutate");
            assert!(!result["isError"].as_bool().unwrap_or(false), "{result}");
        }
        assert_eq!(
            host.calls(),
            [
                "ops.probe",
                "ops.probe",
                "ops.probe",
                "ops.mutate",
                "ops.mutate"
            ]
        );
    }

    /// `orbit mcp serve` for one test, killed and reaped on drop.
    struct McpServer {
        child: Child,
        stdin: ChildStdin,
        lines: mpsc::Receiver<String>,
        next_id: u64,
    }

    impl McpServer {
        fn start(host: &Host, flags: &[&str], env: &[(&str, &str)]) -> Self {
            let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_orbit"));
            test_env::clear_inherited_authority(|name| {
                command.env_remove(name);
            });
            let mut child = command
                .env_remove("AGENT_RUN_ID")
                .envs(env.iter().copied())
                .current_dir(&host.work)
                .env("HOME", &host.home)
                .env("USERPROFILE", &host.home)
                .args(["mcp", "serve"])
                .args(flags)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("start the MCP server");
            let stdin = child.stdin.take().expect("server stdin");
            let stdout = child.stdout.take().expect("server stdout");
            let (sender, lines) = mpsc::channel();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            });
            let mut server = Self {
                child,
                stdin,
                lines,
                next_id: 0,
            };
            let workspace = host.work.to_str().expect("utf8 work");
            server.request(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "plugin-authority", "version": "1" },
                    "_meta": { "orbit": { "workspace": workspace } },
                }),
            );
            server.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
            server
        }

        fn send(&mut self, message: &Value) {
            writeln!(self.stdin, "{message}").expect("write to the server");
            self.stdin.flush().expect("flush the server");
        }

        /// The `result` of one request, waiting at most [`CHILD_TIMEOUT`].
        fn request(&mut self, method: &str, params: Value) -> Value {
            self.next_id += 1;
            let id = self.next_id;
            self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
            let deadline = std::time::Instant::now() + CHILD_TIMEOUT;
            loop {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                let line = self
                    .lines
                    .recv_timeout(remaining)
                    .unwrap_or_else(|error| panic!("no reply to {method}: {error}"));
                let response: Value = serde_json::from_str(&line).expect("JSON-RPC line");
                if response["id"] == id {
                    assert!(response.get("error").is_none(), "{method}: {response}");
                    return response["result"].clone();
                }
            }
        }

        fn call(&mut self, tool: &str) -> Value {
            self.request("tools/call", json!({ "name": tool, "arguments": {} }))
        }
    }

    impl Drop for McpServer {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
