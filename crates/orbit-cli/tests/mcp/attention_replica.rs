//! Real replica and owner attention candidates through the federated mux.
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::{Value, json};

const CONFIG: &str =
    "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n";

fn session(capability: McpCapability) -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: [capability].into(),
        ..Default::default()
    }
}
fn call(runtime: &OrbitRuntime, input: Value, session: &ToolSessionContext) -> Value {
    runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.attention",
            input,
            None,
            None,
            ToolEntryPoint::Mcp,
            session.clone(),
        )
        .unwrap()
        .value
}

#[test]
fn replica_notice_candidates_merge_with_owner_as_all_clear() {
    if crate::isolated_test::run_in_child(
        "attention_replica::replica_notice_candidates_merge_with_owner_as_all_clear",
    ) {
        return;
    }
    use orbit_common::OrbitError;
    use orbit_mcp::McpHost;
    use orbit_mcp::federated::{
        Destination, DestinationProbe, DestinationSnapshot, FederatedMcpHost, RoutedSession,
    };
    use std::sync::Arc;

    #[derive(Clone)]
    struct Probe {
        owner: Value,
        replica: Value,
        workspace: orbit_types::workspace::Workspace,
    }
    struct Route {
        body: Value,
        machine_id: String,
        workspace: orbit_types::workspace::Workspace,
    }
    impl DestinationProbe for Probe {
        fn probe(&self, destination: &Destination) -> Result<DestinationSnapshot, OrbitError> {
            Ok(DestinationSnapshot {
                machine_id: destination.machine_id.clone(),
                workspaces: vec![self.workspace.clone()],
                ..Default::default()
            })
        }
        fn open_route(
            &self,
            destination: &Destination,
        ) -> Result<Box<dyn RoutedSession>, OrbitError> {
            Ok(Box::new(Route {
                body: if destination.machine_id == "hm_owner" {
                    self.owner.clone()
                } else {
                    self.replica.clone()
                },
                machine_id: destination.machine_id.clone(),
                workspace: self.workspace.clone(),
            }))
        }
    }
    impl RoutedSession for Route {
        fn snapshot(&mut self) -> Result<DestinationSnapshot, OrbitError> {
            Ok(DestinationSnapshot {
                machine_id: self.machine_id.clone(),
                workspaces: vec![self.workspace.clone()],
                ..Default::default()
            })
        }
        fn advertised_tools(&mut self) -> Result<Vec<String>, OrbitError> {
            Ok(vec!["orbit_attention".into()])
        }
        fn supports_tool_argument(&mut self, _: &str, _: &str) -> Result<bool, OrbitError> {
            Ok(true)
        }
        fn call_tool(
            &mut self,
            _: &str,
            input: Value,
            _: ToolSessionContext,
        ) -> Result<Value, OrbitError> {
            assert_eq!(input["candidate_read"], true);
            Ok(self.body.clone())
        }
    }

    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let orbit = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&orbit).unwrap();
    std::fs::write(orbit.join("config.toml"), CONFIG).unwrap();
    let owner = OrbitRuntime::from_roots(&global, &orbit).unwrap();
    let replica = owner
        .clone()
        .with_coordination_write_owner(Some("hm_owner".into()));
    let operator = session(McpCapability::Operator);
    let mut aggregator = operator.clone();
    aggregator.mcp_client_name = Some("orbit-federated-mux".into());
    let input = json!({"kind":"notice_open", "candidate_read":true});
    let replica_body = call(&replica, input.clone(), &aggregator);
    let direct = call(&replica, json!({"kind":"notice_open"}), &operator);
    assert_eq!(direct["degraded"][0]["kind"], "notice_open", "{direct}");
    assert_eq!(
        direct["degraded"][0]["code"], "source_unavailable",
        "{direct}"
    );
    assert_eq!(replica_body["degraded"], json!([]), "{replica_body}");
    let owner_body = call(&owner, input, &aggregator);
    assert_eq!(owner_body["degraded"], json!([]), "{owner_body}");
    let mux = FederatedMcpHost::new(
        vec![
            Destination::local("hm_replica", "replica"),
            Destination::ssh("owner.example", "hm_owner"),
        ],
        Arc::new(Probe {
            owner: owner_body,
            replica: replica_body,
            workspace: orbit_types::workspace::Workspace {
                id: owner.workspace_id().unwrap(),
                name: "repo".into(),
                owner_machine_id: Some("hm_owner".into()),
                git_remote: None,
                ship_mode: None,
                base_branch: "main".into(),
                status: orbit_types::workspace::WorkspaceStatus::Active,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
        }),
    );
    let merged = mux
        .call_tool(
            "orbit_attention",
            json!({"kind":"notice_open"}),
            ToolSessionContext::default(),
        )
        .unwrap();
    assert_eq!(merged["items"], json!([]), "{merged}");
    assert_eq!(merged["degraded"], json!([]), "{merged}");
    assert!(
        merged["message"]
            .as_str()
            .unwrap()
            .starts_with("Nothing needs attention"),
        "{merged}"
    );

    // Retained friction on the replica checkout: the replica candidate stays
    // silent, a direct replica read names the owner, and the owner+replica
    // merge carries the owner's single item with no stale duplicate.
    let friction = owner
        .execute_tool_command_dispatch_with_session_context(
            "orbit.friction",
            json!({"action": "add", "body": "Retained friction body", "model": "codex"}),
            None,
            None,
            ToolEntryPoint::Mcp,
            operator.clone(),
        )
        .unwrap()
        .value;
    let friction_input = json!({"kind":"friction_untriaged", "candidate_read":true});
    let replica_friction = call(&replica, friction_input.clone(), &aggregator);
    assert_eq!(replica_friction["items"], json!([]), "{replica_friction}");
    assert_eq!(
        replica_friction["degraded"],
        json!([]),
        "{replica_friction}"
    );
    let direct_friction = call(&replica, json!({"kind":"friction_untriaged"}), &operator);
    assert_eq!(
        direct_friction["degraded"][0]["kind"], "friction_untriaged",
        "{direct_friction}"
    );
    assert_eq!(
        direct_friction["degraded"][0]["code"], "source_unavailable",
        "{direct_friction}"
    );
    assert!(
        direct_friction["degraded"][0]["message"]
            .as_str()
            .unwrap()
            .contains("read the workspace owner"),
        "{direct_friction}"
    );
    let owner_friction = call(&owner, friction_input, &aggregator);
    assert_eq!(
        owner_friction["items"].as_array().unwrap().len(),
        1,
        "{owner_friction}"
    );
    let friction_mux = FederatedMcpHost::new(
        vec![
            Destination::local("hm_replica", "replica"),
            Destination::ssh("owner.example", "hm_owner"),
        ],
        Arc::new(Probe {
            owner: owner_friction,
            replica: replica_friction,
            workspace: orbit_types::workspace::Workspace {
                id: owner.workspace_id().unwrap(),
                name: "repo".into(),
                owner_machine_id: Some("hm_owner".into()),
                git_remote: None,
                ship_mode: None,
                base_branch: "main".into(),
                status: orbit_types::workspace::WorkspaceStatus::Active,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
        }),
    );
    let merged_friction = friction_mux
        .call_tool(
            "orbit_attention",
            json!({"kind":"friction_untriaged"}),
            ToolSessionContext::default(),
        )
        .unwrap();
    let merged_items = merged_friction["items"].as_array().unwrap();
    assert_eq!(merged_items.len(), 1, "{merged_friction}");
    assert_eq!(
        merged_items[0]["refs"]["friction_id"], friction["id"],
        "{merged_friction}"
    );
    assert_eq!(merged_friction["degraded"], json!([]), "{merged_friction}");
}
