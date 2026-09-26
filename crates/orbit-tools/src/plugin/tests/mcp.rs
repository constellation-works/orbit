//! The `mcp` backend against the fixture server: spawned once per backend,
//! verified against the manifest, and never a hang when the child dies.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use orbit_types::plugin::{PluginExecutionKind, PluginGrant, PluginGrantSet, PluginPermissions};
use serde_json::{Value, json};

use super::super::loader::load_plugin_dir;
use super::super::mcp::{McpBackend, McpExpectedTool};
use super::super::schema::CompiledSchema;
use super::super::tool::{PluginBackend, PluginTool, PluginToolBinding};
use super::support::{context, provenance, require_sandbox};
use crate::{Tool, ToolContext};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins/mcp-example")
}

fn require_python3() {
    let found = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|dir| dir.join("python3").is_file());
    assert!(found, "python3 is required by the MCP sandbox fixture");
}

struct Fixture {
    backend: Arc<McpBackend>,
    root: PathBuf,
    timeout_ms: u64,
    _host_root: tempfile::TempDir,
}

impl Fixture {
    fn new(env: &[(&str, &str)], timeout_ms: u64) -> Self {
        Self::build(env, timeout_ms, None, Vec::new())
    }

    /// A backend granted `orbit_tools`, so `ORBIT_ALLOWED_TOOLS` is the
    /// caller's intersection rather than always empty.
    fn with_orbit_tools(orbit_tools: &[&str], timeout_ms: u64) -> Self {
        let permissions = PluginPermissions {
            orbit_tools: orbit_tools.iter().map(|tool| (*tool).to_string()).collect(),
            ..PluginPermissions::default()
        };
        Self::build(
            &[],
            timeout_ms,
            Some(permissions),
            vec![PluginGrant::OrbitTools],
        )
    }

    fn build(
        env: &[(&str, &str)],
        timeout_ms: u64,
        permissions: Option<PluginPermissions>,
        grants: Vec<PluginGrant>,
    ) -> Self {
        let plugin = load_plugin_dir(&fixture_root()).expect("fixture loads");
        let host_root = tempfile::tempdir().expect("temporary plugin host root");
        let global_root = host_root.path().join("global");
        let state_dir = global_root.join("state/plugins/mcpdemo");
        // The `orbit_tools` store directories under this temporary global root
        // are host-owned: Orbit materializes them at spawn. A fixture that
        // copies that inventory here goes stale the next time it changes,
        // which is how this path reddened CI four times [ORB-12872].
        let spec = Arc::new(super::super::backend::PluginBackendSpec {
            provenance: provenance(&grants),
            plugin_root: plugin.root.clone(),
            state_dir,
            global_root,
            command: plugin.backend_command.clone(),
            args: Vec::new(),
            timeout_ms: Some(timeout_ms),
            sandbox: plugin.manifest.spec.backend.sandbox,
            permissions: permissions.unwrap_or_else(|| plugin.manifest.spec.permissions.clone()),
            programs: Vec::new(),
            program_paths: Default::default(),
            config: super::super::backend::PluginConfigSection::new(
                json!({ "index_dir": "/srv/graph", "max_nodes": 500 }),
            ),
            grants: PluginGrantSet::from_grants(grants),
        });
        let expected = plugin
            .tools
            .iter()
            .map(|tool| McpExpectedTool {
                verb: tool.verb.clone(),
                input_schema: tool
                    .input_schema_declared
                    .then(|| tool.input_schema.clone()),
            })
            .collect();
        let _ = env;
        Self {
            backend: Arc::new(McpBackend::new(spec, expected)),
            root: plugin.root,
            timeout_ms,
            _host_root: host_root,
        }
    }

    fn tool(&self, verb: &str, output_schema: Option<Value>) -> PluginTool {
        PluginTool {
            name: format!("mcpdemo.{verb}"),
            verb: verb.to_string(),
            description: String::new(),
            parameters: Vec::new(),
            input_schema: None,
            execution_kind: PluginExecutionKind::ReadOnly,
            output_schema: output_schema
                .map(|schema| CompiledSchema::compile(schema).expect("compile output_schema")),
            binding: Arc::new(PluginToolBinding {
                provenance: provenance(&[]),
                execution_kind: PluginExecutionKind::ReadOnly,
                diagnostic: None,
            }),
            backend: PluginBackend::Mcp(Arc::clone(&self.backend)),
        }
    }

    fn context(&self, env: &[(&str, &str)]) -> ToolContext {
        let mut environment = vec![(
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string()),
        )];
        environment.extend(
            env.iter()
                .map(|(key, value)| (key.to_string(), value.to_string())),
        );
        ToolContext {
            proc_spawn_environment: Some(environment),
            ..context(&self.root)
        }
    }

    /// A caller working in `workspace`: the child's working directory, its
    /// `ORBIT_WORKSPACE_ROOT`, and what the sandbox profile renders
    /// `{{workspace}}` from all follow it.
    fn workspace_context(&self, workspace: &Path) -> ToolContext {
        ToolContext {
            workspace_root: Some(workspace.to_path_buf()),
            ..self.context(&[])
        }
    }

    fn context_with_allowed(&self, allowed: &[&str]) -> ToolContext {
        let mut ctx = self.context(&[]);
        ctx.allowed_tools = allowed.iter().map(|tool| (*tool).to_string()).collect();
        ctx
    }

    /// The `effective_tools` ceiling recorded on every live callback session
    /// under this fixture's host root, sorted so the set is comparable.
    fn recorded_session_ceilings(&self) -> Vec<Vec<String>> {
        let dir = self._host_root.path().join("global/state/plugin-callbacks");
        let mut ceilings: Vec<Vec<String>> = std::fs::read_dir(&dir)
            .expect("the host minted a callback session directory")
            .flatten()
            .filter_map(|entry| std::fs::read(entry.path()).ok())
            .filter_map(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .map(|record| {
                record["effective_tools"]
                    .as_array()
                    .expect("every session record states its ceiling")
                    .iter()
                    .map(|tool| tool.as_str().expect("tool name").to_string())
                    .collect()
            })
            .collect();
        ceilings.sort();
        ceilings
    }
}

#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn the_server_is_spawned_once_and_serves_every_call() {
    require_sandbox();
    require_python3();
    let fixture = Fixture::new(&[], 5_000);
    let ctx = fixture.context(&[]);
    assert!(
        !fixture.backend.is_running(),
        "nothing runs before the first call"
    );

    let echo = fixture.tool(
        "echo",
        Some(json!({ "type": "object", "required": ["echo", "pid"] })),
    );
    let first = echo
        .execute(&ctx, json!({ "message": "one" }))
        .expect("first call");
    assert_eq!(first["echo"]["message"], "one");
    assert_eq!(first["plugin"], "demo", "the child carries the plugin env");
    let pid = fixture
        .backend
        .child_pid(&ctx)
        .expect("running after the first call");
    assert_eq!(first["pid"].as_u64(), Some(u64::from(pid)));

    let second = echo
        .execute(&ctx, json!({ "message": "two" }))
        .expect("second call");
    assert_eq!(
        second["pid"], first["pid"],
        "one server per allowed-tools intersection"
    );
    assert_eq!(fixture.backend.child_pid(&ctx), Some(pid));

    // The proxied output is still held to `output_schema`.
    let strict = fixture.tool(
        "echo",
        Some(json!({ "type": "object", "required": ["missing_field"] })),
    );
    let error = strict.execute(&ctx, json!({})).unwrap_err().to_string();
    assert!(error.contains("violates its output_schema"), "{error}");
}

#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn a_manifest_mismatch_refuses_startup_naming_the_tool() {
    require_sandbox();
    require_python3();
    let fixture = Fixture::new(&[], 5_000);
    let ctx = fixture.context(&[("MCP_FIXTURE_EXTRA_TOOL", "surprise")]);
    let error = fixture
        .tool("echo", None)
        .execute(&ctx, json!({}))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("refused to start") && error.contains("'surprise'"),
        "{error}"
    );
    assert!(
        !fixture.backend.is_running(),
        "the refused server is not kept"
    );
    // Refusal is sticky for this runtime: no respawn, same diagnostic.
    let again = fixture
        .tool("echo", None)
        .execute(&fixture.context(&[]), json!({}))
        .unwrap_err()
        .to_string();
    assert!(again.contains("'surprise'"), "{again}");

    let fixture = Fixture::new(&[], 5_000);
    let ctx = fixture.context(&[(
        "MCP_FIXTURE_ECHO_SCHEMA",
        "{\"type\":\"object\",\"properties\":{\"text\":{\"type\":\"string\"}}}",
    )]);
    let error = fixture
        .tool("echo", None)
        .execute(&ctx, json!({}))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("tool 'echo'") && error.contains("different input schema"),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn a_dead_or_stuck_child_is_a_timely_tool_error_and_is_respawned() {
    require_sandbox();
    require_python3();
    let fixture = Fixture::new(&[], 2_000);
    let ctx = fixture.context(&[]);
    let echo = fixture.tool("echo", None);
    echo.execute(&ctx, json!({})).expect("warm up");
    let first_pid = fixture.backend.child_pid(&ctx).expect("running");

    // The child exits mid-call: the error names it and arrives at once.
    let started = Instant::now();
    let error = fixture
        .tool("crash", None)
        .execute(&ctx, json!({}))
        .unwrap_err()
        .to_string();
    assert!(error.contains("mcp server"), "{error}");
    assert!(
        started.elapsed() < Duration::from_millis(fixture.timeout_ms),
        "a dead child must not wait out the timeout: {:?}",
        started.elapsed()
    );
    assert!(!fixture.backend.is_running());

    // The next call respawns a fresh server.
    let output = echo.execute(&ctx, json!({})).expect("respawned");
    let second_pid = fixture.backend.child_pid(&ctx).expect("running again");
    assert_ne!(first_pid, second_pid);
    assert_eq!(output["pid"].as_u64(), Some(u64::from(second_pid)));

    // Killed from outside mid-call: same outcome.
    let doomed = fixture
        .backend
        .child_pid(&ctx)
        .expect("running before the slow call");
    let killer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        // SAFETY: `doomed` is the fixture server this test spawned.
        unsafe { libc_kill(doomed) };
    });
    let started = Instant::now();
    let error = fixture
        .tool("slow", None)
        .execute(&ctx, json!({ "seconds": 30 }))
        .unwrap_err()
        .to_string();
    killer.join().expect("killer thread");
    assert!(error.contains("mcp server"), "{error}");
    assert!(
        started.elapsed() < Duration::from_millis(fixture.timeout_ms),
        "a killed child must not wait out the timeout: {:?}",
        started.elapsed()
    );

    // A child that merely stops answering is ended at the timeout.
    let started = Instant::now();
    let error = fixture
        .tool("slow", None)
        .execute(&ctx, json!({ "seconds": 30 }))
        .unwrap_err()
        .to_string();
    assert!(error.contains("within the timeout"), "{error}");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(fixture.timeout_ms)
            && elapsed < Duration::from_millis(fixture.timeout_ms + 3_000),
        "the timeout is the bound, not a hang: {elapsed:?}"
    );
    assert!(!fixture.backend.is_running(), "the stuck child was killed");
}

#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn a_narrower_caller_does_not_inherit_the_wider_session() {
    require_sandbox();
    require_python3();
    // Wide first is the ordering that used to leak: the first caller's
    // intersection became the ceiling for everyone who followed.
    let fixture = Fixture::with_orbit_tools(&["orbit.task.show", "orbit.search"], 5_000);
    let wide = fixture.context_with_allowed(&["orbit.task.show", "orbit.search"]);
    let also_wide = fixture.context_with_allowed(&["orbit.search", "orbit.task.show"]);
    let narrow = fixture.context_with_allowed(&["orbit.task.show"]);
    let echo = fixture.tool("echo", None);

    let first = echo.execute(&wide, json!({})).expect("wide caller");
    assert_eq!(
        first["allowed"], "orbit.task.show,orbit.search",
        "the first session is spawned with the less-restricted intersection"
    );
    let wide_pid = first["pid"].clone();

    let second = echo.execute(&narrow, json!({})).expect("narrow caller");
    assert_ne!(
        second["pid"], wide_pid,
        "a narrower caller must not inherit the wider session"
    );
    assert_eq!(
        second["allowed"], "orbit.task.show",
        "the narrower session is spawned with its own intersection"
    );

    let third = echo
        .execute(
            &fixture.context_with_allowed(&["orbit.task.show"]),
            json!({}),
        )
        .expect("identical narrow intersection");
    assert_eq!(
        third["pid"], second["pid"],
        "two callers with the same intersection share a session"
    );

    let fourth = echo
        .execute(&also_wide, json!({}))
        .expect("same wide intersection");
    assert_eq!(
        fourth["pid"], wide_pid,
        "the original wide session is still reused for the same intersection"
    );

    // Separate children are only half the isolation: each one's host-owned
    // callback session must record *its* caller's intersection, because that
    // record — not `ORBIT_ALLOWED_TOOLS`, which the child can rewrite — is
    // what bounds the callbacks it makes [ORB-12801].
    assert_eq!(
        fixture.recorded_session_ceilings(),
        vec![
            vec!["orbit.search".to_string(), "orbit.task.show".to_string()],
            vec!["orbit.task.show".to_string()],
        ],
        "each live mcp session carries its own caller's ceiling"
    );
}

/// The child's working directory as the child itself reports it: a temporary
/// root can sit behind a symbolic link (`/var` on macOS), and the kernel
/// answers `getcwd` with the resolved path.
fn resolved(path: &Path) -> String {
    std::fs::canonicalize(path)
        .expect("workspace root exists")
        .to_string_lossy()
        .into_owned()
}

#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn a_second_workspace_gets_its_own_child_rather_than_the_first_one_s() {
    require_sandbox();
    require_python3();
    // One backend, two workspaces — what `orbit clock tick` and `orbit mcp
    // serve` are. Keyed by the allowed-tools intersection alone, both
    // workspaces shared one child confined to whichever called first, so the
    // plugin mutated workspace A for a request made about B [ORB-12820].
    let fixture = Fixture::new(&[], 5_000);
    let first_root = tempfile::tempdir().expect("first workspace");
    let second_root = tempfile::tempdir().expect("second workspace");
    let first = fixture.workspace_context(first_root.path());
    let second = fixture.workspace_context(second_root.path());
    let echo = fixture.tool("echo", None);

    let from_first = echo.execute(&first, json!({})).expect("first workspace");
    let from_second = echo.execute(&second, json!({})).expect("second workspace");

    assert_ne!(
        from_first["pid"], from_second["pid"],
        "a second workspace must not be proxied to the first workspace's child"
    );
    assert_eq!(
        from_first["workspace"],
        json!(first_root.path().to_string_lossy()),
        "each child is told the workspace it was spawned for"
    );
    assert_eq!(
        from_second["workspace"],
        json!(second_root.path().to_string_lossy())
    );
    assert_eq!(
        from_first["cwd"],
        json!(resolved(first_root.path())),
        "and is confined to it by its working directory too"
    );
    assert_eq!(from_second["cwd"], json!(resolved(second_root.path())));

    // Both sessions are live at once, and each context still finds its own.
    assert_eq!(
        fixture.backend.child_pid(&first).map(u64::from),
        from_first["pid"].as_u64()
    );
    assert_eq!(
        fixture.backend.child_pid(&second).map(u64::from),
        from_second["pid"].as_u64()
    );
    let again = echo
        .execute(&first, json!({}))
        .expect("the first workspace calls again");
    assert_eq!(
        again["pid"], from_first["pid"],
        "the same workspace still shares one child"
    );
}

#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn tools_call_carries_this_call_s_context_not_the_session_s() {
    require_sandbox();
    require_python3();
    let fixture = Fixture::new(&[], 5_000);
    let workspace = tempfile::tempdir().expect("workspace");
    let mut ctx = fixture.workspace_context(workspace.path());
    ctx.agent_name = Some("claude".to_string());
    ctx.model_name = Some("opus-5".to_string());
    let echo = fixture.tool("echo", None);

    let first = echo.execute(&ctx, json!({})).expect("first call");
    assert_eq!(
        first["meta"]["orbit"],
        json!({
            "workspace_root": workspace.path().to_string_lossy(),
            "agent": "claude",
            "model": "opus-5",
            "config": { "index_dir": "/srv/graph", "max_nodes": 500 },
            "tool": "mcpdemo.echo",
        }),
        "`tools/call` carries the context the exec envelope carries — including \
         the plugin's effective config section — plus the tool name the shared \
         child has no `ORBIT_TOOL_NAME` for"
    );

    // The point of sending it per call: one child serves every caller that
    // shares its key, so the environment cannot say who this call is for.
    let mut other = ctx.clone();
    other.agent_name = Some("codex".to_string());
    other.model_name = Some("gpt-5".to_string());
    let second = echo.execute(&other, json!({})).expect("second caller");
    assert_eq!(
        second["pid"], first["pid"],
        "the two callers share one session"
    );
    assert_eq!(second["meta"]["orbit"]["agent"], "codex");
    assert_eq!(second["meta"]["orbit"]["model"], "gpt-5");
}

#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn a_server_request_before_the_reply_is_answered_rather_than_dropped() {
    require_sandbox();
    require_python3();
    // The fixture asks the host something mid-`tools/call` and blocks on the
    // answer. A client that skips every message that is not its own response
    // never sends one, and the call it was about to receive dies at the
    // deadline with the child killed as unresponsive [ORB-12820].
    let fixture = Fixture::new(&[], 3_000);
    let ctx = fixture.context(&[("MCP_FIXTURE_SERVER_REQUEST", "ping")]);
    let started = Instant::now();
    let output = fixture
        .tool("echo", None)
        .execute(&ctx, json!({}))
        .expect("a server ping is answered, so the call completes");
    assert!(
        started.elapsed() < Duration::from_millis(fixture.timeout_ms),
        "answering the ping must not cost the timeout: {:?}",
        started.elapsed()
    );
    assert_eq!(output["answer"]["id"], "fixture-server-1");
    assert_eq!(output["answer"]["result"], json!({}));
    assert!(fixture.backend.is_running(), "and the child is kept");

    // Orbit declares no capabilities, so anything else is method-not-found —
    // an answer, which is what keeps the call moving.
    let fixture = Fixture::new(&[], 3_000);
    let ctx = fixture.context(&[("MCP_FIXTURE_SERVER_REQUEST", "roots/list")]);
    let output = fixture
        .tool("echo", None)
        .execute(&ctx, json!({}))
        .expect("an unsupported server request still completes");
    assert_eq!(output["answer"]["error"]["code"], -32601);
}

#[cfg(unix)]
#[test]
#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]
fn one_session_s_slow_call_does_not_hold_up_another_session() {
    require_sandbox();
    require_python3();
    /// Long enough that serialising two of them is unmistakable, short
    /// enough to stay well inside the backend timeout below.
    const SLEEP_SECONDS: u64 = 2;
    let fixture = Fixture::new(&[], 30_000);
    let first_root = tempfile::tempdir().expect("first workspace");
    let second_root = tempfile::tempdir().expect("second workspace");
    let first = fixture.workspace_context(first_root.path());
    let second = fixture.workspace_context(second_root.path());

    // Warm both sessions, so what is being timed is the proxied call and not
    // two handshakes.
    let echo = fixture.tool("echo", None);
    echo.execute(&first, json!({})).expect("warm the first");
    echo.execute(&second, json!({})).expect("warm the second");

    let started = Instant::now();
    let fixture = &fixture;
    std::thread::scope(|scope| {
        for ctx in [&first, &second] {
            scope.spawn(move || {
                fixture
                    .tool("slow", None)
                    .execute(ctx, json!({ "seconds": SLEEP_SECONDS }))
                    .expect("slow call");
            });
        }
    });
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(SLEEP_SECONDS * 1_000 + 1_500),
        "two sessions of one backend must overlap, not queue behind a single \
         state lock: {elapsed:?} for two {SLEEP_SECONDS}s calls"
    );
}

/// `kill -9 <pid>` through the shell, so the test crate needs no libc.
unsafe fn libc_kill(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
}
