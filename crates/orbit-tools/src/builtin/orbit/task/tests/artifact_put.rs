//! Execution and source path resolution tests for task.artifact_put.
//
// Migrated from nested `task/artifact_put/tests/` (anti-pattern child of source)
// to sibling layout under `task/tests/` per ORB-00243 and
// docs/design-patterns/test_layout.md.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use orbit_common::OrbitError;
use orbit_types::task::MAX_TASK_ARTIFACT_CONTENT_BYTES;
use orbit_types::tool::{McpCapability, McpTransport, RemoteCallerGrant, ToolSessionContext};

use super::super::artifact_put::*;
use crate::{OrbitBuiltinAction, OrbitTaskScope, OrbitToolHost, Tool, ToolContext};

#[derive(Clone, Default)]
struct RecordingHost {
    call: Arc<Mutex<Option<RecordedCall>>>,
}

#[derive(Debug)]
struct RecordedCall {
    action: OrbitBuiltinAction,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
}

impl OrbitToolHost for RecordingHost {
    fn execute(
        &self,
        action: OrbitBuiltinAction,
        input: Value,
        agent: Option<String>,
        model: Option<String>,
        _reservation_owner: Option<crate::ReservationOwnerContext>,
    ) -> Result<Value, OrbitError> {
        *self.call.lock().expect("record call") = Some(RecordedCall {
            action,
            input,
            agent,
            model,
        });
        Ok(json!({ "ok": true }))
    }

    fn task_scope(&self) -> OrbitTaskScope {
        OrbitTaskScope::default()
    }
}

fn context_in(dir: &Path, host: RecordingHost) -> ToolContext {
    ToolContext {
        cwd: Some(dir.to_string_lossy().into_owned()),
        workspace_root: Some(dir.to_path_buf()),
        orbit_host: Some(Arc::new(host)),
        ..Default::default()
    }
}

fn assert_invalid_input(error: OrbitError) {
    assert!(
        matches!(error, OrbitError::InvalidInput(_)),
        "expected invalid_input, got {error}"
    );
}

#[test]
fn artifact_put_reads_relative_source_and_delegates_to_task_update() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("summary.md");
    std::fs::write(&source, "done\n").expect("write source");
    let host = RecordingHost::default();
    let ctx = context_in(dir.path(), host.clone());

    let output = OrbitTaskArtifactPutTool
        .execute(
            &ctx,
            json!({
                "id": "ORB-00001",
                "source_path": "summary.md",
                "path": "reports/summary.md",
                "model": "gpt-5"
            }),
        )
        .expect("execute tool");

    assert_eq!(output, json!({ "ok": true }));
    let call = host.call.lock().expect("recorded call").take().unwrap();
    assert_eq!(call.action, OrbitBuiltinAction::TaskUpdate);
    assert_eq!(call.agent.as_deref(), Some("codex"));
    assert_eq!(call.model.as_deref(), Some("gpt-5"));
    assert_eq!(call.input["id"], "ORB-00001");
    assert_eq!(call.input["artifacts"][0]["path"], "reports/summary.md");
    assert_eq!(
        call.input["artifacts"][0]["content"],
        json!([100, 111, 110, 101, 10])
    );
    assert!(call.input.get("source_path").is_none());
}

#[test]
fn artifact_put_rejects_agent_identity_field() {
    let ctx = ToolContext::default();
    let error = OrbitTaskArtifactPutTool
        .execute(
            &ctx,
            json!({
                "id": "ORB-00001",
                "source_path": "summary.md",
                "agent": "codex",
            }),
        )
        .expect_err("agent must be rejected before reading the source file");

    assert!(error.to_string().contains("use `model`"));
}

#[test]
fn artifact_put_read_failure_never_calls_host() {
    let dir = tempfile::tempdir().expect("tempdir");
    let host = RecordingHost::default();
    let ctx = context_in(dir.path(), host.clone());
    let error = OrbitTaskArtifactPutTool
        .execute(
            &ctx,
            json!({"id": "ORB-00001", "source_path": "definitely-missing"}),
        )
        .expect_err("missing source must fail locally");

    assert!(error.to_string().contains("read artifact source"));
    assert!(host.call.lock().expect("host call").is_none());
}

#[test]
fn artifact_put_size_failure_never_calls_host() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("large.bin");
    std::fs::write(
        &source,
        vec![0_u8; (MAX_TASK_ARTIFACT_CONTENT_BYTES + 1) as usize],
    )
    .expect("write oversized source");
    let host = RecordingHost::default();
    let ctx = context_in(dir.path(), host.clone());
    let error = OrbitTaskArtifactPutTool
        .execute(&ctx, json!({"id": "ORB-00001", "source_path": source}))
        .expect_err("oversized source must fail locally");

    assert!(error.to_string().contains("content limit"));
    assert!(host.call.lock().expect("host call").is_none());
}

#[test]
fn artifact_put_rejects_source_outside_workspace_root() {
    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    let source = outside.path().join("secret.txt");
    std::fs::write(&source, "leaked\n").expect("write outside source");
    let host = RecordingHost::default();
    let ctx = context_in(workspace.path(), host.clone());
    let error = OrbitTaskArtifactPutTool
        .execute(&ctx, json!({"id": "ORB-00001", "source_path": source}))
        .expect_err("outside workspace_root must be refused");

    assert_invalid_input(error);
    assert!(host.call.lock().expect("host call").is_none());
}

#[cfg(unix)]
#[test]
fn artifact_put_rejects_symlink_inside_workspace_pointing_outside() {
    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    let secret = outside.path().join("secret.txt");
    std::fs::write(&secret, "leaked\n").expect("write outside target");
    let link = workspace.path().join("link.txt");
    std::os::unix::fs::symlink(&secret, &link).expect("symlink");
    let host = RecordingHost::default();
    let ctx = context_in(workspace.path(), host.clone());
    let error = OrbitTaskArtifactPutTool
        .execute(&ctx, json!({"id": "ORB-00001", "source_path": "link.txt"}))
        .expect_err("escaping symlink must be refused");

    assert_invalid_input(error);
    assert!(host.call.lock().expect("host call").is_none());
}

#[test]
fn artifact_put_accepts_file_inside_workspace_root() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("notes.md"), "ok\n").expect("write inside source");
    let host = RecordingHost::default();
    let ctx = context_in(workspace.path(), host.clone());

    OrbitTaskArtifactPutTool
        .execute(
            &ctx,
            json!({
                "id": "ORB-00001",
                "source_path": "notes.md",
                "model": "codex"
            }),
        )
        .expect("in-workspace source must be accepted");

    let call = host.call.lock().expect("recorded call").take().unwrap();
    assert_eq!(call.action, OrbitBuiltinAction::TaskUpdate);
    assert_eq!(call.input["artifacts"][0]["path"], "notes.md");
}

#[test]
fn remote_agent_session_cannot_attach_mcp_ssh_acceptance_secret() {
    let workspace = tempfile::tempdir().expect("workspace");
    let orbit_home = tempfile::tempdir().expect("orbit home");
    let acceptance_dir = orbit_home.path().join("mcp-ssh-acceptance");
    std::fs::create_dir_all(&acceptance_dir).expect("acceptance dir");
    let secret = acceptance_dir.join("hm_caller.toml");
    std::fs::write(&secret, "capability = \"secret\"\n").expect("write acceptance secret");

    let host = RecordingHost::default();
    let ctx = ToolContext {
        cwd: Some(workspace.path().to_string_lossy().into_owned()),
        workspace_root: Some(workspace.path().to_path_buf()),
        session_context: ToolSessionContext {
            transport: Some(McpTransport::SshMcp),
            effective_capabilities: BTreeSet::from([McpCapability::Agent]),
            remote_caller_grant: Some(RemoteCallerGrant {
                caller_machine_id: "hm_caller".to_string(),
                granted_capabilities: BTreeSet::from([McpCapability::Agent]),
                source: secret.display().to_string(),
                ..RemoteCallerGrant::default()
            }),
            ..ToolSessionContext::default()
        },
        orbit_host: Some(Arc::new(host.clone())),
        ..ToolContext::default()
    };

    let error = OrbitTaskArtifactPutTool
        .execute(
            &ctx,
            json!({
                "id": "ORB-00001",
                "source_path": secret,
                "path": "hm_caller.toml",
                "model": "codex"
            }),
        )
        .expect_err("remote agent must not attach mcp-ssh-acceptance secrets");

    assert_invalid_input(error);
    assert!(host.call.lock().expect("host call").is_none());
}

#[test]
fn preloaded_artifact_payload_is_private_to_authenticated_ssh_mcp() {
    let host = RecordingHost::default();
    let ctx = ToolContext {
        session_context: ToolSessionContext {
            transport: Some(McpTransport::SshMcp),
            ..ToolSessionContext::default()
        },
        orbit_host: Some(Arc::new(host.clone())),
        ..ToolContext::default()
    };
    OrbitTaskArtifactPutTool
        .execute(
            &ctx,
            json!({
                "id": "ORB-00001",
                "artifacts": [{"path": "reports/result.txt", "content": [111, 107]}],
                "model": "codex"
            }),
        )
        .expect("authenticated hub accepts path-free connector payload");
    let call = host.call.lock().expect("recorded call").take().unwrap();
    assert_eq!(call.action, OrbitBuiltinAction::TaskUpdate);
    assert_eq!(call.input["artifacts"][0]["path"], "reports/result.txt");

    let local = ToolContext {
        orbit_host: Some(Arc::new(RecordingHost::default())),
        ..ToolContext::default()
    };
    let error = OrbitTaskArtifactPutTool
        .execute(
            &local,
            json!({
                "id": "ORB-00001",
                "artifacts": [{"path": "reports/result.txt", "content": [111, 107]}]
            }),
        )
        .expect_err("ordinary local/model calls cannot inject the private payload");
    assert!(error.to_string().contains("authenticated ssh-mcp"));
}
