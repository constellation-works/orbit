//! Source-path confinement for `orbit.task.artifact.put`.

use std::sync::{Arc, Mutex};

use serde_json::json;

use orbit_common::OrbitError;
use orbit_types::tool::{McpCapability, McpTransport, ToolSessionContext};

use super::super::artifact_put::OrbitTaskArtifactPutTool;
use crate::{OrbitBuiltinAction, OrbitTaskScope, OrbitToolHost, Tool, ToolContext};
use std::collections::BTreeSet;

#[derive(Clone, Default)]
struct RecordingHost {
    calls: Arc<Mutex<usize>>,
}

impl OrbitToolHost for RecordingHost {
    fn execute(
        &self,
        _action: OrbitBuiltinAction,
        _input: serde_json::Value,
        _agent: Option<String>,
        _model: Option<String>,
        _reservation_owner: Option<crate::ReservationOwnerContext>,
    ) -> Result<serde_json::Value, OrbitError> {
        *self.calls.lock().expect("record call") += 1;
        Ok(json!({ "ok": true }))
    }

    fn task_scope(&self) -> OrbitTaskScope {
        OrbitTaskScope::default()
    }
}

fn assert_invalid_input(error: OrbitError) {
    assert!(
        matches!(error, OrbitError::InvalidInput(_)),
        "expected invalid_input, got {error}"
    );
}

#[test]
fn remote_agent_session_cannot_attach_a_host_secret_outside_the_workspace() {
    let workspace = tempfile::tempdir().expect("workspace");
    let orbit_home = tempfile::tempdir().expect("orbit home");
    let ssh_dir = orbit_home.path().join(".ssh");
    std::fs::create_dir_all(&ssh_dir).expect("ssh dir");
    let secret = ssh_dir.join("id_ed25519");
    std::fs::write(&secret, "PRIVATE KEY\n").expect("write host secret");

    let host = RecordingHost::default();
    let ctx = ToolContext {
        cwd: Some(workspace.path().to_string_lossy().into_owned()),
        workspace_root: Some(workspace.path().to_path_buf()),
        session_context: ToolSessionContext {
            transport: Some(McpTransport::SshMcp),
            effective_capabilities: BTreeSet::from([McpCapability::Agent]),
            caller_machine_id: Some("hm_caller".to_string()),
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
                "path": "id_ed25519",
                "model": "codex"
            }),
        )
        .expect_err("remote agent must not attach host secrets outside the workspace");

    assert_invalid_input(error);
    assert_eq!(*host.calls.lock().expect("host call"), 0);
}
