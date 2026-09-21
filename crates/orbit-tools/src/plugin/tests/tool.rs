use std::path::PathBuf;
use std::sync::Arc;

use orbit_types::plugin::{PluginExecutionKind, PluginProvenance};
use serde_json::json;

use super::super::tool::{PluginTool, PluginToolBinding};
use crate::{Tool, ToolContext, ToolExecutionKind};

fn stub_backend(dir: &std::path::Path, script: &str) -> PathBuf {
    let path = dir.join("backend.sh");
    std::fs::write(&path, script).expect("write backend");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    path
}

fn tool(command: PathBuf, root: PathBuf) -> PluginTool {
    PluginTool {
        name: "demo.hello".into(),
        description: "demo".into(),
        parameters: vec![],
        execution_kind: PluginExecutionKind::ReadOnly,
        binding: Arc::new(PluginToolBinding {
            provenance: PluginProvenance {
                name: "demo".into(),
                version: "1.0.0".into(),
                manifest_digest: "abc".into(),
            },
            execution_kind: PluginExecutionKind::ReadOnly,
            diagnostic: None,
        }),
        plugin_root: root.clone(),
        state_dir: root.join("state"),
        command,
        args: vec!["--serve".into()],
        timeout_ms: Some(5_000),
        requested_orbit_tools: vec!["orbit.task.show".into()],
    }
}

fn context(cwd: &std::path::Path) -> ToolContext {
    ToolContext {
        cwd: Some(cwd.to_string_lossy().into_owned()),
        allowed_tools: vec!["orbit.task.show".into(), "demo.hello".into()],
        ..ToolContext::default()
    }
}

#[cfg(unix)]
#[test]
fn exec_backend_receives_the_envelope_and_returns_output() {
    let temp = tempfile::tempdir().expect("tempdir");
    let command = stub_backend(
        temp.path(),
        "#!/bin/sh\ninput=$(cat)\nprintf '{\"ok\":true,\"output\":{\"arg\":\"%s\",\"plugin\":\"%s\",\"allowed\":\"%s\",\"envelope\":%s}}\\n' \"$1\" \"$ORBIT_PLUGIN\" \"$ORBIT_ALLOWED_TOOLS\" \"$input\"\n",
    );
    let tool = tool(command, temp.path().to_path_buf());
    assert_eq!(tool.execution_kind(), ToolExecutionKind::ReadOnly);
    let output = tool
        .execute(&context(temp.path()), json!({ "name": "world" }))
        .expect("backend succeeds");
    assert_eq!(output["arg"], "--serve");
    assert_eq!(output["plugin"], "demo");
    assert_eq!(output["allowed"], "orbit.task.show");
    assert_eq!(output["envelope"]["schema_version"], 1);
    assert_eq!(output["envelope"]["tool"], "demo.hello");
    assert_eq!(output["envelope"]["input"]["name"], "world");
}

#[cfg(unix)]
#[test]
fn exec_backend_failures_are_tool_errors() {
    let temp = tempfile::tempdir().expect("tempdir");
    let command = stub_backend(
        temp.path(),
        "#!/bin/sh\ncat >/dev/null\nprintf '{\"ok\":false,\"error\":{\"code\":\"nope\",\"message\":\"declined\"}}\\n'\n",
    );
    let error = tool(command, temp.path().to_path_buf())
        .execute(&context(temp.path()), json!({}))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("nope") && error.contains("declined"),
        "{error}"
    );

    let command = stub_backend(temp.path(), "#!/bin/sh\ncat >/dev/null\necho not-json\n");
    let error = tool(command, temp.path().to_path_buf())
        .execute(&context(temp.path()), json!({}))
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid JSON output"), "{error}");

    let command = stub_backend(
        temp.path(),
        "#!/bin/sh\ncat >/dev/null\necho boom >&2\nexit 3\n",
    );
    let error = tool(command, temp.path().to_path_buf())
        .execute(&context(temp.path()), json!({}))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("exited with 3") && error.contains("boom"),
        "{error}"
    );
}
