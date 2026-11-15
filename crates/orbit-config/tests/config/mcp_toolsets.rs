// [ORB-22711] The retired `agent-invoke` MCP toolset stays loadable: a
// stored `machine.mcp.toolsets` entry naming it is ignored with a warning,
// so hosts that opted in before ORB-20383 keep resolving config after the
// toolset's removal.

fn mcp_toolsets_write_global(root: &Path, body: &str) {
    std::fs::create_dir_all(root).expect("global config directory");
    std::fs::write(root.join("config.toml"), body).expect("global config");
}

fn mcp_toolsets_write_workspace(root: &Path, body: &str) {
    std::fs::create_dir_all(root).expect("workspace config directory");
    std::fs::write(root.join("config.toml"), body).expect("workspace config");
}

#[derive(Clone)]
struct McpToolsetsCapture(Arc<Mutex<Vec<u8>>>);

impl Write for McpToolsetsCapture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("capture").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for McpToolsetsCapture {
    type Writer = McpToolsetsCapture;

    fn make_writer(&self) -> Self::Writer {
        self.clone()
    }
}

fn mcp_toolsets_load_with_logs(roots: &ConfigRoots) -> (ResolvedConfig, String) {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(McpToolsetsCapture(buffer.clone()))
        .with_ansi(false)
        .finish();
    let config = tracing::subscriber::with_default(subscriber, || ResolvedConfig::load(roots))
        .expect("retired toolset must not fail config resolution");
    let logs = String::from_utf8(buffer.lock().expect("logs").clone()).expect("utf8 logs");
    (config, logs)
}

#[test]
fn retired_agent_invoke_toolset_is_ignored_with_a_warning() {
    let global = tempfile::tempdir().expect("global directory");
    let workspace = tempfile::tempdir().expect("workspace directory");
    let roots = ConfigRoots::new(global.path(), workspace.path());
    mcp_toolsets_write_global(
        global.path(),
        "[machine.mcp]\ntoolsets = [\"agent-invoke\"]\n",
    );
    mcp_toolsets_write_workspace(workspace.path(), "");

    let (config, logs) = mcp_toolsets_load_with_logs(&roots);
    assert!(
        !config
            .snapshot
            .machine_mcp_toolsets
            .iter()
            .any(|name| name.trim() == "agent-invoke"),
        "the retired entry is ignored: {:?} ({logs})",
        config.snapshot.machine_mcp_toolsets
    );
    assert!(
        logs.contains("agent-invoke") && logs.contains("removed"),
        "the load warns naming the removal: {logs}"
    );
}

#[test]
fn retired_agent_invoke_direct_parse_names_the_removal() {
    let error = "agent-invoke"
        .parse::<orbit_types::tool::McpToolset>()
        .expect_err("a direct parse names the removal instead of unknown toolset");
    let message = error.to_string();
    assert!(
        message.contains("was removed") && message.contains("agent-invoke"),
        "the refusal names the removal: {message}"
    );
}

#[test]
fn retired_agent_invoke_toolset_mixed_with_a_live_toolset_keeps_the_live_one() {
    let global = tempfile::tempdir().expect("global directory");
    let workspace = tempfile::tempdir().expect("workspace directory");
    let roots = ConfigRoots::new(global.path(), workspace.path());
    mcp_toolsets_write_global(
        global.path(),
        "[machine.mcp]\ntoolsets = [\"agent-invoke\", \"leases\"]\n",
    );
    mcp_toolsets_write_workspace(workspace.path(), "");

    let (config, logs) = mcp_toolsets_load_with_logs(&roots);
    assert_eq!(
        config.snapshot.machine_mcp_toolsets,
        vec!["leases".to_string()],
        "only the live entry survives ({logs})"
    );
}
