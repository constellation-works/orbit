use orbit_types::plugin::{PluginGrant, PluginPermissions};
use serde_json::json;

use super::support::{context, sandbox_unavailable, spec, stub_backend, tool};
use crate::{Tool, ToolContext, ToolExecutionKind};

const ECHO_BACKEND: &str = "#!/bin/sh\ninput=$(cat)\nprintf '{\"ok\":true,\"output\":{\"arg\":\"%s\",\"plugin\":\"%s\",\"allowed\":\"%s\",\"programs\":\"%s\",\"envelope\":%s}}\\n' \"$1\" \"$ORBIT_PLUGIN\" \"$ORBIT_ALLOWED_TOOLS\" \"$ORBIT_PROC_ALLOWED_PROGRAMS\" \"$input\"\n";

fn orbit_tools_permissions() -> PluginPermissions {
    PluginPermissions {
        orbit_tools: vec!["orbit.task.show".into(), "orbit.search".into()],
        ..PluginPermissions::default()
    }
}

#[cfg(unix)]
#[test]
fn exec_backend_receives_the_envelope_and_returns_output() {
    if sandbox_unavailable() {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let command = stub_backend(temp.path(), ECHO_BACKEND);
    let tool = tool(
        spec(
            command,
            temp.path(),
            orbit_tools_permissions(),
            &[PluginGrant::OrbitTools],
        ),
        None,
    );
    assert_eq!(tool.execution_kind(), ToolExecutionKind::ReadOnly);
    let ctx = ToolContext {
        allowed_tools: vec!["orbit.task.show".into(), "demo.hello".into()],
        ..context(temp.path())
    };
    let output = tool
        .execute(&ctx, json!({ "name": "world" }))
        .expect("backend succeeds");
    assert_eq!(output["arg"], "--serve");
    assert_eq!(output["plugin"], "demo");
    // Requested ∩ granted ∩ the caller's own allowlist.
    assert_eq!(output["allowed"], "orbit.task.show");
    assert_eq!(output["envelope"]["schema_version"], 1);
    assert_eq!(output["envelope"]["tool"], "demo.hello");
    assert_eq!(output["envelope"]["input"]["name"], "world");
}

#[cfg(unix)]
#[test]
fn the_callback_allowlist_is_exactly_the_granted_orbit_tools() {
    if sandbox_unavailable() {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let command = stub_backend(temp.path(), ECHO_BACKEND);

    // Granted, no caller allowlist: every requested tool.
    let granted = tool(
        spec(
            command.clone(),
            temp.path(),
            orbit_tools_permissions(),
            &[PluginGrant::OrbitTools],
        ),
        None,
    );
    let output = granted
        .execute(&context(temp.path()), json!({}))
        .expect("backend succeeds");
    assert_eq!(output["allowed"], "orbit.task.show,orbit.search");

    // Requested but not granted: the variable is present and empty, so the
    // child's `orbit tool run` refuses everything.
    let ungranted = tool(
        spec(command, temp.path(), orbit_tools_permissions(), &[]),
        None,
    );
    let output = ungranted
        .execute(&context(temp.path()), json!({}))
        .expect("backend succeeds");
    assert_eq!(output["allowed"], "");
}

#[cfg(unix)]
#[test]
fn exec_backend_failures_are_tool_errors_with_no_partial_output() {
    if sandbox_unavailable() {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let permissions = PluginPermissions::default();

    let command = stub_backend(
        temp.path(),
        "#!/bin/sh\ncat >/dev/null\nprintf '{\"ok\":false,\"error\":{\"code\":\"nope\",\"message\":\"declined\"}}\\n'\n",
    );
    let error = tool(spec(command, temp.path(), permissions.clone(), &[]), None)
        .execute(&context(temp.path()), json!({}))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("nope") && error.contains("declined"),
        "{error}"
    );

    let command = stub_backend(temp.path(), "#!/bin/sh\ncat >/dev/null\necho not-json\n");
    let error = tool(spec(command, temp.path(), permissions.clone(), &[]), None)
        .execute(&context(temp.path()), json!({}))
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid JSON output"), "{error}");

    let command = stub_backend(
        temp.path(),
        "#!/bin/sh\ncat >/dev/null\necho boom >&2\nexit 3\n",
    );
    let error = tool(spec(command, temp.path(), permissions.clone(), &[]), None)
        .execute(&context(temp.path()), json!({}))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("exited with 3") && error.contains("boom"),
        "{error}"
    );

    // A response that parses but violates `output_schema` never reaches the
    // caller either.
    let command = stub_backend(
        temp.path(),
        "#!/bin/sh\ncat >/dev/null\nprintf '{\"ok\":true,\"output\":{\"count\":\"three\"}}\\n'\n",
    );
    let schema = json!({
        "type": "object",
        "required": ["count"],
        "properties": { "count": { "type": "integer" } }
    });
    let error = tool(
        spec(command.clone(), temp.path(), permissions.clone(), &[]),
        Some(schema.clone()),
    )
    .execute(&context(temp.path()), json!({}))
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("violates its output_schema") && error.contains("count"),
        "{error}"
    );
    let command = stub_backend(
        temp.path(),
        "#!/bin/sh\ncat >/dev/null\nprintf '{\"ok\":true,\"output\":{\"count\":3}}\\n'\n",
    );
    let output = tool(spec(command, temp.path(), permissions, &[]), Some(schema))
        .execute(&context(temp.path()), json!({}))
        .expect("valid output passes the schema");
    assert_eq!(output["count"], 3);
}
