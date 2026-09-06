//! Execution and source path resolution tests for task.artifact_put.
//
// Migrated from nested `task/artifact_put/tests/` (anti-pattern child of source)
// to sibling layout under `task/tests/` per ORB-00243 and
// docs/design-patterns/test_layout.md.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use orbit_common::OrbitError;
use orbit_types::task::MAX_TASK_ARTIFACT_CONTENT_BYTES;
use orbit_types::tool::{McpTransport, ToolSessionContext};

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

#[test]
fn artifact_put_reads_relative_source_and_delegates_to_task_update() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("summary.md");
    std::fs::write(&source, "done\n").expect("write source");
    let host = RecordingHost::default();
    let ctx = ToolContext {
        cwd: Some(dir.path().to_string_lossy().into_owned()),
        orbit_host: Some(Arc::new(host.clone())),
        ..Default::default()
    };

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
    let host = RecordingHost::default();
    let ctx = ToolContext {
        orbit_host: Some(Arc::new(host.clone())),
        ..Default::default()
    };
    let error = OrbitTaskArtifactPutTool
        .execute(
            &ctx,
            json!({"id": "ORB-00001", "source_path": "/definitely/missing"}),
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
    let ctx = ToolContext {
        orbit_host: Some(Arc::new(host.clone())),
        ..Default::default()
    };
    let error = OrbitTaskArtifactPutTool
        .execute(&ctx, json!({"id": "ORB-00001", "source_path": source}))
        .expect_err("oversized source must fail locally");

    assert!(error.to_string().contains("content limit"));
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
