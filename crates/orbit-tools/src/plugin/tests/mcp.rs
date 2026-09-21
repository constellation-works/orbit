//! The `mcp` backend against the fixture server: spawned once per backend,
//! verified against the manifest, and never a hang when the child dies.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use orbit_types::plugin::PluginExecutionKind;
use serde_json::{Value, json};

use super::super::loader::load_plugin_dir;
use super::super::mcp::{McpBackend, McpExpectedTool};
use super::super::tool::{PluginBackend, PluginTool, PluginToolBinding};
use super::support::{context, provenance, sandbox_unavailable};
use crate::{Tool, ToolContext};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins/mcp-example")
}

#[allow(clippy::print_stderr)]
fn python3_missing() -> bool {
    let found = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|dir| dir.join("python3").is_file());
    if !found {
        eprintln!("skipping: python3 is not on PATH");
    }
    !found
}

struct Fixture {
    backend: Arc<McpBackend>,
    root: PathBuf,
    timeout_ms: u64,
}

impl Fixture {
    fn new(env: &[(&str, &str)], timeout_ms: u64) -> Self {
        let plugin = load_plugin_dir(&fixture_root()).expect("fixture loads");
        let spec = Arc::new(super::super::backend::PluginBackendSpec {
            provenance: provenance(&[]),
            plugin_root: plugin.root.clone(),
            state_dir: plugin.root.join("state"),
            global_root: plugin.root.join("global"),
            command: plugin.backend_command.clone(),
            args: Vec::new(),
            timeout_ms: Some(timeout_ms),
            sandbox: plugin.manifest.spec.backend.sandbox,
            permissions: plugin.manifest.spec.permissions.clone(),
            programs: Vec::new(),
            config_defaults: Default::default(),
            grants: Vec::new(),
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
        }
    }

    fn tool(&self, verb: &str, output_schema: Option<Value>) -> PluginTool {
        PluginTool {
            name: format!("mcpdemo.{verb}"),
            verb: verb.to_string(),
            description: String::new(),
            parameters: Vec::new(),
            execution_kind: PluginExecutionKind::ReadOnly,
            output_schema,
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
}

#[cfg(unix)]
#[test]
fn the_server_is_spawned_once_and_serves_every_call() {
    if sandbox_unavailable() || python3_missing() {
        return;
    }
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
        .child_pid()
        .expect("running after the first call");
    assert_eq!(first["pid"].as_u64(), Some(u64::from(pid)));

    let second = echo
        .execute(&ctx, json!({ "message": "two" }))
        .expect("second call");
    assert_eq!(second["pid"], first["pid"], "one server per runtime");
    assert_eq!(fixture.backend.child_pid(), Some(pid));

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
fn a_manifest_mismatch_refuses_startup_naming_the_tool() {
    if sandbox_unavailable() || python3_missing() {
        return;
    }
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
fn a_dead_or_stuck_child_is_a_timely_tool_error_and_is_respawned() {
    if sandbox_unavailable() || python3_missing() {
        return;
    }
    let fixture = Fixture::new(&[], 2_000);
    let ctx = fixture.context(&[]);
    let echo = fixture.tool("echo", None);
    echo.execute(&ctx, json!({})).expect("warm up");
    let first_pid = fixture.backend.child_pid().expect("running");

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
    let second_pid = fixture.backend.child_pid().expect("running again");
    assert_ne!(first_pid, second_pid);
    assert_eq!(output["pid"].as_u64(), Some(u64::from(second_pid)));

    // Killed from outside mid-call: same outcome.
    let backend = Arc::clone(&fixture.backend);
    let killer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        let pid = backend.child_pid().expect("running during the slow call");
        // SAFETY: `pid` is the fixture server this test spawned.
        unsafe { libc_kill(pid) };
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

/// `kill -9 <pid>` through the shell, so the test crate needs no libc.
unsafe fn libc_kill(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
}
