//! Fail-closed federated routing through the exported mux.
//!
//! `FederatedMcpHost` and `DestinationProbe` are the public seams. The
//! scripted probe supplies destination answers without SSH.

mod workspace_route {
    use super::*;

    fn destinations() -> Vec<Destination> {
        vec![
            destination("orbit-owner", OWNER_MACHINE),
            destination("operator@orbit-replica", REPLICA_MACHINE),
            destination("orbit-down", "hm_down"),
        ]
    }

    fn owner_snapshot() -> DestinationSnapshot {
        DestinationSnapshot {
            crews: Default::default(),
            machine_id: OWNER_MACHINE.to_string(),
            workspaces: vec![workspace("ws_orbit", Some(OWNER_MACHINE))],
            host: Default::default(),
        }
    }

    fn replica_snapshot() -> DestinationSnapshot {
        DestinationSnapshot {
            crews: Default::default(),
            machine_id: REPLICA_MACHINE.to_string(),
            workspaces: vec![workspace("ws_orbit", Some(OWNER_MACHINE))],
            host: Default::default(),
        }
    }

    fn three_destination_probe() -> ScriptedProbe {
        ScriptedProbe::new()
            .answering(OWNER_MACHINE, owner_snapshot())
            .answering(REPLICA_MACHINE, replica_snapshot())
            .refusing(
                "hm_down",
                OrbitError::UnreachableDestination("hm_down: could not start SSH".to_string()),
            )
    }

    fn routed_mux() -> (FederatedMcpHost, crate::scripted_destination::CallLog) {
        let probe = three_destination_probe();
        let log = probe.call_log();
        (FederatedMcpHost::new(destinations(), Arc::new(probe)), log)
    }

    fn call(host: &FederatedMcpHost, tool: &str, selector: &str) -> Result<Value, OrbitError> {
        host.call_tool(
            tool,
            json!({ "workspace": selector }),
            ToolSessionContext::default(),
        )
    }

    fn call_err(host: &FederatedMcpHost, tool: &str, selector: &str) -> OrbitError {
        call(host, tool, selector).expect_err("expected a routing failure")
    }

    #[test]
    fn unknown_selectors_fail_before_any_destination() {
        let (host, log) = routed_mux();

        for selector in ["ws_orbit", "orbit-linux/ws_orbit", "hm_unknown/ws_orbit"] {
            let error = call_err(&host, "orbit.workflow.run.list", selector);
            assert!(
                matches!(error, OrbitError::UnknownSelector(_)),
                "{selector}: {error}"
            );
        }
        assert!(
            log.calls().is_empty(),
            "a token that is not a configured hm_*/ws_* must not touch a destination"
        );
    }

    #[test]
    fn a_malformed_explicit_selector_is_refused_instead_of_using_the_session_default() {
        let (host, log) = routed_mux();
        let context = ToolSessionContext {
            workspace: Some("hm_owner/ws_orbit".to_string()),
            ..ToolSessionContext::default()
        };

        for malformed in [
            json!(42),
            json!(true),
            json!(["hm_owner/ws_orbit"]),
            json!({}),
        ] {
            let error = host
                .call_tool(
                    "orbit.task.update",
                    json!({ "id": "ORB-1", "workspace": malformed, "comment": "hi" }),
                    context.clone(),
                )
                .expect_err("a non-string workspace must be refused");
            assert!(
                matches!(error, OrbitError::InvalidInput(_)),
                "{malformed}: {error:?}"
            );
        }
        assert!(
            log.calls().is_empty(),
            "a malformed selector must neither contact a destination nor dispatch a mutation"
        );

        for omitted in [
            json!({ "id": "ORB-1" }),
            json!({ "id": "ORB-1", "workspace": null }),
        ] {
            host.call_tool("orbit.task.update", omitted, context.clone())
                .expect("an omitted or null workspace uses the session default");
        }
        assert_eq!(log.calls().len(), 2, "the defaults still dispatch");
    }

    #[test]
    fn a_dispatched_call_whose_answer_is_lost_is_outcome_unknown_not_unreachable() {
        let probe = three_destination_probe().on_call(
            OWNER_MACHINE,
            "orbit.task.add",
            ScriptedToolResult::PostDispatchTimeout,
        );
        let log = probe.call_log();
        let host = FederatedMcpHost::new(destinations(), Arc::new(probe));

        let error = call_err(&host, "orbit.task.add", "hm_owner/ws_orbit");
        assert!(
            matches!(error, OrbitError::OutcomeUnknown { .. }),
            "the mux must not fold a possibly-committed remote write into the delivery-miss class \
             a caller retries: {error}"
        );
        let calls = log.calls();
        assert_eq!(
            calls.len(),
            1,
            "the call was delivered once and is not retried here: {calls:?}"
        );
    }

    /// A destination reached whose store stayed locked decided nothing; it
    /// is not a host the caller lost. The mux keeps its `lock_busy` class, so
    /// a bound worker retries the read and a follower drain backs off rather
    /// than suppressing its own host.
    #[test]
    fn a_destination_whose_store_stayed_locked_is_lock_busy_through_the_mux() {
        let probe = three_destination_probe().store_locked(OWNER_MACHINE);
        let log = probe.call_log();
        let host = FederatedMcpHost::new(destinations(), Arc::new(probe));

        let error = call_err(&host, "orbit.task.show", "hm_owner/ws_orbit");
        assert!(error.is_lock_busy(), "{error:?}");
        assert!(
            !matches!(error, OrbitError::UnreachableDestination(_)),
            "{error:?}"
        );
        assert!(log.calls().is_empty(), "nothing was dispatched");
    }
}

mod task_route {
    use super::*;

    const LOCAL_MACHINE: &str = "hm_local";
    const DOWN_MACHINE: &str = "hm_down";

    fn snapshot(machine_id: &str, workspaces: &[(&str, &str)]) -> DestinationSnapshot {
        DestinationSnapshot {
            crews: Default::default(),
            machine_id: machine_id.to_string(),
            workspaces: workspaces
                .iter()
                .map(|(id, name)| {
                    let mut row = workspace(id, Some(machine_id));
                    row.name = name.to_string();
                    row
                })
                .collect(),
            host: Default::default(),
        }
    }

    fn entry(name: &str, machine_id: &str, prefix: &str) -> HostEntry {
        HostEntry {
            name: name.to_string(),
            machine_id: machine_id.to_string(),
            ssh: name.to_string(),
            task_prefix: prefix.to_string(),
            host_os: None,
            offline: None,
        }
    }

    fn destinations() -> Vec<Destination> {
        vec![
            Destination::local(LOCAL_MACHINE, "local-box"),
            destination("owner", OWNER_MACHINE),
            destination("down", DOWN_MACHINE),
        ]
    }

    fn table() -> TaskPrefixTable {
        TaskPrefixTable::new(
            Some("LB".to_string()),
            vec![
                entry("owner", OWNER_MACHINE, "OWN"),
                entry("down", DOWN_MACHINE, "DWN"),
            ],
        )
    }

    fn probe() -> ScriptedProbe {
        ScriptedProbe::new()
            .answering(
                LOCAL_MACHINE,
                snapshot(LOCAL_MACHINE, &[("ws_local", "local")]),
            )
            .answering(
                OWNER_MACHINE,
                snapshot(
                    OWNER_MACHINE,
                    &[
                        ("ws_orbit", "orbit"),
                        ("ws_twin1", "twin"),
                        ("ws_twin2", "twin"),
                    ],
                ),
            )
            .refusing(
                DOWN_MACHINE,
                OrbitError::UnreachableDestination("hm_down: could not start SSH".into()),
            )
    }

    fn routed(
        mirror: Option<&'static str>,
    ) -> (FederatedMcpHost, crate::scripted_destination::CallLog) {
        let probe = probe();
        let log = probe.call_log();
        let mut host = FederatedMcpHost::new(destinations(), Arc::new(probe))
            .with_task_prefix_routing(table());
        if let Some(workspace) = mirror {
            host = host.with_local_mirror_hint(Arc::new(move |_| Some(workspace.to_string())));
        }
        (host, log)
    }

    fn call(host: &FederatedMcpHost, tool: &str, input: Value) -> Result<Value, OrbitError> {
        host.call_tool(tool, input, ToolSessionContext::default())
    }

    #[test]
    fn an_id_only_repilot_routes_to_the_owner_and_refuses_unknown_prefixes() {
        use orbit_types::tool::McpCapability;
        let (host, log) = routed(None);
        let mut operator = ToolSessionContext::default();
        operator
            .effective_capabilities
            .insert(McpCapability::Operator);
        let response = host
            .call_tool(
                "orbit.task.recover",
                json!({"action": "repilot", "id": "OWN-7"}),
                operator.clone(),
            )
            .expect("repilot must reach the owner without a workspace selector");
        assert_eq!(response["id"], "OWN-7");
        assert_eq!(
            log.calls(),
            vec![(
                OWNER_MACHINE.to_string(),
                "orbit.task.recover".to_string(),
                json!({"action": "repilot", "id": "OWN-7"})
            )]
        );
        let error = host
            .call_tool(
                "orbit.task.recover",
                json!({"action": "repilot", "id": "ZZZ-1"}),
                operator,
            )
            .expect_err("an unknown prefix must not search or fall back to another host");
        assert_eq!(
            error.host_registry_code(),
            Some(HostRegistryCode::UnknownTaskPrefix)
        );
        assert_eq!(
            log.calls().len(),
            1,
            "refusal reaches no additional destination"
        );
    }

    #[test]
    fn an_id_only_call_goes_to_the_host_its_prefix_names_without_a_selector() {
        let (host, log) = routed(None);

        let remote = call(
            &host,
            "orbit.task.update",
            json!({"id": "OWN-7", "comment": "hi"}),
        )
        .expect("remote prefix delivered");
        let local = call(&host, "orbit.task.show", json!({"id": "LB-3"})).expect("local prefix");

        assert_eq!(
            log.calls(),
            vec![
                (
                    OWNER_MACHINE.to_string(),
                    "orbit.task.update".to_string(),
                    json!({"id": "OWN-7", "comment": "hi"}),
                ),
                (
                    LOCAL_MACHINE.to_string(),
                    "orbit.task.show".to_string(),
                    json!({"id": "LB-3"}),
                ),
            ],
            "each id reaches only its prefix's host, and the destination resolves it itself"
        );
        assert_eq!(remote["id"], "OWN-7");
        assert_eq!(local["id"], "LB-3");
    }

    #[test]
    fn an_explicit_selector_wins_over_the_prefix() {
        let (host, log) = routed(None);

        call(
            &host,
            "orbit.task.show",
            json!({"id": "OWN-7", "workspace": format!("{LOCAL_MACHINE}/ws_local")}),
        )
        .expect("selector route");

        let calls = log.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(
            calls[0].0, LOCAL_MACHINE,
            "the selector's host, not the prefix's"
        );
        assert_eq!(calls[0].2["workspace"], "ws_local");
    }

    #[test]
    fn an_unregistered_prefix_is_refused_before_any_destination() {
        let (host, log) = routed(None);

        let error = call(&host, "orbit.task.show", json!({"id": "ZZZ-1"})).expect_err("refused");

        assert_eq!(
            error.host_registry_code(),
            Some(HostRegistryCode::UnknownTaskPrefix),
            "{error}"
        );
        assert!(log.calls().is_empty(), "routing never searches hosts");
    }

    #[test]
    fn an_unreachable_holder_is_owner_unreachable_and_names_the_local_mirror() {
        let (host, log) = routed(Some("orbit-mirror"));

        let error = call(&host, "orbit.task.show", json!({"id": "DWN-4"})).expect_err("refused");

        assert_eq!(
            error.host_registry_code(),
            Some(HostRegistryCode::OwnerUnreachable),
            "{error}"
        );
        assert!(
            error.to_string().contains("--workspace orbit-mirror"),
            "the error says how to read the mirror explicitly: {error}"
        );
        assert!(
            log.calls().is_empty(),
            "no mirror fallback: {:?}",
            log.calls()
        );
    }

    #[test]
    fn host_workspace_selector_copies_the_listed_selector() {
        let (host, _) = routed(None);

        assert_eq!(
            host.host_workspace_selector(OWNER_MACHINE, "orbit")
                .expect("by name"),
            format!("{OWNER_MACHINE}/ws_orbit")
        );
        assert_eq!(
            host.host_workspace_selector(OWNER_MACHINE, "ws_orbit")
                .expect("by id"),
            format!("{OWNER_MACHINE}/ws_orbit")
        );

        let stale = host
            .host_workspace_selector(OWNER_MACHINE, "missing")
            .expect_err("not listed");
        assert!(matches!(stale, OrbitError::StaleRoute(_)), "{stale}");
        assert!(
            stale.to_string().contains("orbit (ws_orbit)"),
            "stale_route lists what the host does list: {stale}"
        );
        let ambiguous = host
            .host_workspace_selector(OWNER_MACHINE, "twin")
            .expect_err("two matches");
        assert!(
            matches!(ambiguous, OrbitError::UnknownSelector(_)),
            "{ambiguous}"
        );
        let down = host
            .host_workspace_selector(DOWN_MACHINE, "orbit")
            .expect_err("no answer");
        assert!(
            matches!(down, OrbitError::UnreachableDestination(_)),
            "{down}"
        );
    }

    #[test]
    fn task_id_alias_canonicalizes_to_id_and_refuses_conflict() {
        let (host, log) = routed(None);

        let remote = call(
            &host,
            "orbit.task.update",
            json!({"task_id": "OWN-7", "comment": "hi"}),
        )
        .expect("remote prefix delivered via task_id");
        let local =
            call(&host, "orbit.task.show", json!({"task_id": "LB-3"})).expect("local prefix");
        let artifact = call(
            &host,
            "orbit.task.artifact",
            json!({"action": "get", "task_id": "OWN-7", "path": "a.txt"}),
        )
        .expect("artifact get via task_id");
        let workspace_routed = call(
            &host,
            "orbit.task.show",
            json!({"workspace": format!("{OWNER_MACHINE}/ws_orbit"), "task_id": "OWN-7"}),
        )
        .expect("workspace-selected call canonicalizes task_id to id");
        let non_aliased = call(
            &host,
            "orbit.task.eligible",
            json!({"workspace": format!("{OWNER_MACHINE}/ws_orbit"), "task_id": "OWN-7"}),
        )
        .expect("non-aliased tool keeps task_id unchanged");

        assert_eq!(remote["id"], "OWN-7");
        assert!(remote.get("task_id").is_none(), "{remote}");
        assert_eq!(local["id"], "LB-3");
        assert!(local.get("task_id").is_none(), "{local}");
        assert_eq!(artifact["id"], "OWN-7");
        assert!(artifact.get("task_id").is_none(), "{artifact}");
        assert_eq!(workspace_routed["id"], "OWN-7");
        assert!(
            workspace_routed.get("task_id").is_none(),
            "{workspace_routed}"
        );
        assert_eq!(non_aliased["task_id"], "OWN-7");
        assert!(non_aliased.get("id").is_none(), "{non_aliased}");
        assert_eq!(
            log.calls(),
            vec![
                (
                    OWNER_MACHINE.to_string(),
                    "orbit.task.update".to_string(),
                    json!({"id": "OWN-7", "comment": "hi"}),
                ),
                (
                    LOCAL_MACHINE.to_string(),
                    "orbit.task.show".to_string(),
                    json!({"id": "LB-3"}),
                ),
                (
                    OWNER_MACHINE.to_string(),
                    "orbit.task.artifact".to_string(),
                    json!({"action": "get", "id": "OWN-7", "path": "a.txt"}),
                ),
                (
                    OWNER_MACHINE.to_string(),
                    "orbit.task.show".to_string(),
                    json!({"workspace": "ws_orbit", "id": "OWN-7"}),
                ),
                (
                    OWNER_MACHINE.to_string(),
                    "orbit.task.eligible".to_string(),
                    json!({"workspace": "ws_orbit", "task_id": "OWN-7"}),
                ),
            ]
        );

        let calls_before = log.calls().len();
        for conflicting in [
            json!({"id": "OWN-7", "task_id": "LB-3"}),
            json!({"id": "OWN-7", "task_id": "OWN-7"}),
            json!({"workspace": format!("{OWNER_MACHINE}/ws_orbit"), "id": "OWN-7", "task_id": "OWN-7"}),
        ] {
            let error = call(&host, "orbit.task.show", conflicting.clone())
                .expect_err("conflicting id and task_id must be refused");
            let message = error.to_string();
            assert!(
                message.contains("id") && message.contains("task_id"),
                "conflict refusal must name both `id` and `task_id` for {conflicting}: {message}"
            );
        }
        assert_eq!(
            log.calls().len(),
            calls_before,
            "conflicting id and task_id must not dispatch to a destination"
        );
    }

    #[test]
    fn id_only_owner_serves_federated_task_reads_for_id_and_task_id() {
        use std::sync::Mutex;

        use orbit_mcp::federated::{DestinationProbe, RoutedSession};
        use orbit_tools::OwnerCoordinator;
        use orbit_types::task::ExecutionLocation;
        use orbit_types::tool::{WorkerInvocation, mcp_advertised_tool_name};

        struct IdOnlyOwnerProbe {
            snapshot: DestinationSnapshot,
            calls: Arc<Mutex<Vec<(String, Value)>>>,
        }

        struct IdOnlyOwnerSession {
            snapshot: DestinationSnapshot,
            calls: Arc<Mutex<Vec<(String, Value)>>>,
        }

        impl DestinationProbe for IdOnlyOwnerProbe {
            fn probe(&self, _destination: &Destination) -> Result<DestinationSnapshot, OrbitError> {
                Ok(self.snapshot.clone())
            }

            fn open_route(
                &self,
                _destination: &Destination,
            ) -> Result<Box<dyn RoutedSession>, OrbitError> {
                Ok(Box::new(IdOnlyOwnerSession {
                    snapshot: self.snapshot.clone(),
                    calls: Arc::clone(&self.calls),
                }))
            }
        }

        impl RoutedSession for IdOnlyOwnerSession {
            fn snapshot(&mut self) -> Result<DestinationSnapshot, OrbitError> {
                Ok(self.snapshot.clone())
            }

            fn advertised_tools(&mut self) -> Result<Vec<String>, OrbitError> {
                Ok(orbit_mcp::canonical_mcp_tool_definitions()
                    .expect("canonical tools")
                    .into_iter()
                    .map(|definition| mcp_advertised_tool_name(&definition.schema.name))
                    .collect())
            }

            fn supports_tool_argument(
                &mut self,
                name: &str,
                argument: &str,
            ) -> Result<bool, OrbitError> {
                Ok(orbit_mcp::canonical_mcp_tool_definitions()
                    .expect("canonical tools")
                    .into_iter()
                    .any(|definition| {
                        definition.schema.name == name
                            && definition
                                .schema
                                .parameters
                                .iter()
                                .any(|parameter| parameter.name == argument)
                    }))
            }

            fn call_tool(
                &mut self,
                name: &str,
                arguments: Value,
                _session_context: ToolSessionContext,
            ) -> Result<Value, OrbitError> {
                self.calls
                    .lock()
                    .expect("calls")
                    .push((name.to_string(), arguments.clone()));
                if arguments.get("task_id").is_some() {
                    return Err(OrbitError::InvalidInput(
                        "unknown parameter `task_id` on preceding id-only owner".into(),
                    ));
                }
                let id = arguments
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| {
                        OrbitError::InvalidInput("missing required parameter `id`".into())
                    })?;
                Ok(json!({ "id": id, "title": "Owner task", "status": "backlog" }))
            }
        }

        let calls = Arc::new(Mutex::new(Vec::new()));
        let probe = IdOnlyOwnerProbe {
            snapshot: snapshot(OWNER_MACHINE, &[("ws_orbit", "orbit")]),
            calls: Arc::clone(&calls),
        };
        let host =
            FederatedMcpHost::new(vec![destination("owner", OWNER_MACHINE)], Arc::new(probe))
                .with_task_prefix_routing(TaskPrefixTable::new(
                    None,
                    vec![entry("owner", OWNER_MACHINE, "OWN")],
                ));

        // 1. Operator tools/list remains unchanged (never advertises `task_id`).
        let definitions = host
            .list_mcp_tool_definitions()
            .expect("operator tools/list");
        for definition in &definitions {
            if definition.schema.name.starts_with("orbit.task.") {
                assert!(
                    definition
                        .schema
                        .parameters
                        .iter()
                        .all(|param| param.name != "task_id"),
                    "{} must not advertise `task_id`",
                    definition.schema.name
                );
            }
        }

        // 2. Prefix-routed reads succeed with either `id` or `task_id`.
        let by_id = call(&host, "orbit.task.show", json!({"id": "OWN-7"}))
            .expect("id spelling on id-only owner");
        let by_alias = call(&host, "orbit.task.show", json!({"task_id": "OWN-7"}))
            .expect("task_id spelling on id-only owner");
        assert_eq!(by_id, by_alias);
        assert_eq!(by_alias["id"], "OWN-7");

        // 3. Explicit workspace selector reads succeed with `task_id`.
        let by_selector = call(
            &host,
            "orbit.task.show",
            json!({"workspace": format!("{OWNER_MACHINE}/ws_orbit"), "task_id": "OWN-7"}),
        )
        .expect("workspace-qualified task_id on id-only owner");
        assert_eq!(by_selector, by_id);

        // 4. Worker owner route (`OwnerCoordinator`) also canonicalizes `task_id`.
        let worker_session = ToolSessionContext {
            worker_invocation: Some(WorkerInvocation {
                owner_machine_id: OWNER_MACHINE.to_string(),
                owner_workspace_id: "ws_orbit".to_string(),
                owner_destination: format!("{OWNER_MACHINE}/ws_orbit"),
                task_id: "OWN-7".to_string(),
                claim_id: "claim-1".to_string(),
                execution: ExecutionLocation {
                    machine_id: REPLICA_MACHINE.to_string(),
                    machine_name: None,
                },
                bound_run_id: "run-1".to_string(),
            }),
            ..ToolSessionContext::default()
        };
        let by_worker = OwnerCoordinator::call(
            &host,
            "orbit.task.show",
            json!({"workspace": format!("{OWNER_MACHINE}/ws_orbit"), "task_id": "OWN-7"}),
            worker_session.clone(),
        )
        .expect("owner coordinator canonicalizes task_id");
        assert_eq!(by_worker, by_id);

        assert_eq!(
            *calls.lock().expect("calls"),
            vec![
                ("orbit.task.show".to_string(), json!({"id": "OWN-7"})),
                ("orbit.task.show".to_string(), json!({"id": "OWN-7"})),
                (
                    "orbit.task.show".to_string(),
                    json!({"workspace": "ws_orbit", "id": "OWN-7"}),
                ),
                (
                    "orbit.task.show".to_string(),
                    json!({"workspace": "ws_orbit", "id": "OWN-7"}),
                ),
            ]
        );

        // 5. Conflicting pair is rejected before dispatch on both routes.
        let calls_before = calls.lock().expect("calls").len();
        let conflict = call(
            &host,
            "orbit.task.show",
            json!({"id": "OWN-7", "task_id": "OWN-8"}),
        )
        .expect_err("conflicting pair must fail");
        assert!(
            conflict.to_string().contains("id") && conflict.to_string().contains("task_id"),
            "{conflict}"
        );
        let worker_conflict = OwnerCoordinator::call(
            &host,
            "orbit.task.show",
            json!({"workspace": format!("{OWNER_MACHINE}/ws_orbit"), "id": "OWN-7", "task_id": "OWN-7"}),
            worker_session,
        )
        .expect_err("conflicting pair on owner route must fail");
        assert!(
            worker_conflict.to_string().contains("id")
                && worker_conflict.to_string().contains("task_id"),
            "{worker_conflict}"
        );
        assert_eq!(
            calls.lock().expect("calls").len(),
            calls_before,
            "conflicting pair must not dispatch"
        );
    }
}

mod notice_aggregation {
    use super::*;
    use orbit_types::workspace::WorkspaceStatus;

    const OTHER_MACHINE: &str = "hm_other";
    const DOWN_MACHINE: &str = "hm_down";

    fn snapshot(
        machine_id: &str,
        workspaces: Vec<orbit_types::workspace::Workspace>,
    ) -> DestinationSnapshot {
        DestinationSnapshot {
            crews: Default::default(),
            machine_id: machine_id.to_string(),
            workspaces,
            host: Default::default(),
        }
    }

    fn notice(id: &str, kind: &str, updated_at: &str) -> Value {
        json!({ "id": id, "kind": kind, "updated_at": updated_at, "title": id })
    }

    fn mux() -> (FederatedMcpHost, crate::scripted_destination::CallLog) {
        let probe = ScriptedProbe::new()
            .answering(
                OWNER_MACHINE,
                snapshot(
                    OWNER_MACHINE,
                    vec![workspace("ws_orbit", Some(OWNER_MACHINE))],
                ),
            )
            .answering(
                OTHER_MACHINE,
                snapshot(
                    OTHER_MACHINE,
                    vec![workspace("ws_other", Some(OTHER_MACHINE))],
                ),
            )
            .answering(
                REPLICA_MACHINE,
                snapshot(
                    REPLICA_MACHINE,
                    vec![workspace("ws_orbit", Some(OWNER_MACHINE))],
                ),
            )
            .refusing(
                DOWN_MACHINE,
                OrbitError::UnreachableDestination("hm_down: could not start SSH".to_string()),
            )
            .on_call(
                OWNER_MACHINE,
                "orbit.notice.list",
                ScriptedToolResult::Answer(json!({
                    "notices": [
                        notice("ntc_info", "info", "2026-11-02T09:00:00Z"),
                        notice("ntc_incident", "incident", "2026-11-02T07:00:00Z"),
                    ],
                    "degraded": [],
                })),
            )
            .on_call(
                OTHER_MACHINE,
                "orbit.notice.list",
                ScriptedToolResult::Answer(json!({
                    "notices": [
                        notice("ntc_incident", "incident", "2026-11-02T07:00:00Z"),
                        notice("ntc_maint", "maintenance", "2026-11-02T08:00:00Z"),
                    ],
                    "degraded": [],
                })),
            );
        let log = probe.call_log();
        let host = FederatedMcpHost::new(
            vec![
                destination("orbit-owner", OWNER_MACHINE),
                destination("orbit-other", OTHER_MACHINE),
                destination("operator@orbit-replica", REPLICA_MACHINE),
                destination("orbit-down", DOWN_MACHINE),
            ],
            Arc::new(probe),
        );
        (host, log)
    }

    #[test]
    fn workspaceless_notice_list_uses_a_healthy_addressable_owner_checkout() {
        let mut broken = workspace("ws_broken", Some(OWNER_MACHINE));
        broken.status = WorkspaceStatus::Invalid;
        let unaddressable = workspace("legacy", Some(OWNER_MACHINE));
        let good = workspace("ws_good", Some(OWNER_MACHINE));
        let probe = ScriptedProbe::new()
            .answering(
                OWNER_MACHINE,
                snapshot(OWNER_MACHINE, vec![broken, unaddressable, good]),
            )
            .on_call(
                OWNER_MACHINE,
                "orbit.notice.list",
                ScriptedToolResult::Answer(json!({
                    "notices": [notice("ntc_owner", "info", "2026-11-02T09:00:00Z")],
                    "degraded": [],
                })),
            );
        let log = probe.call_log();
        let host = FederatedMcpHost::new(
            vec![destination("orbit-owner", OWNER_MACHINE)],
            Arc::new(probe),
        );
        let answer = host
            .call_tool(
                "orbit.notice.list",
                json!({}),
                ToolSessionContext::default(),
            )
            .unwrap();
        assert_eq!(answer["notices"][0]["id"], "ntc_owner", "{answer}");
        assert_eq!(answer["degraded"], json!([]));
        let calls = log.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0].2["workspace"], "ws_good");
        assert_eq!(calls[0].2["all"], true);
    }

    #[test]
    fn workspaceless_notice_list_degrades_an_owner_without_a_usable_checkout() {
        let mut broken = workspace("ws_broken", Some(OWNER_MACHINE));
        broken.status = WorkspaceStatus::Invalid;
        let unaddressable = workspace("legacy", Some(OWNER_MACHINE));
        let probe = ScriptedProbe::new().answering(
            OWNER_MACHINE,
            snapshot(OWNER_MACHINE, vec![broken, unaddressable]),
        );
        let log = probe.call_log();
        let host = FederatedMcpHost::new(
            vec![destination("orbit-owner", OWNER_MACHINE)],
            Arc::new(probe),
        );
        let answer = host
            .call_tool(
                "orbit.notice.list",
                json!({}),
                ToolSessionContext::default(),
            )
            .unwrap();
        assert_eq!(answer["notices"], json!([]));
        assert!(
            answer["degraded"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry.as_str().unwrap().contains(OWNER_MACHINE)),
            "{answer}"
        );
        assert!(log.calls().is_empty());
    }

    /// A workspace-less list reads each owner once with `all: true` and no
    /// limit, then merges: deduplicated by id, in listing order, limited
    /// once. The reachable replica is read through its owner, never
    /// itself; the down host degrades instead of shortening the list.
    #[test]
    fn workspaceless_notice_list_merges_every_owner_once() {
        let (host, log) = mux();
        let answer = host
            .call_tool(
                "orbit.notice.list",
                json!({ "limit": 2 }),
                ToolSessionContext::default(),
            )
            .unwrap();

        let ids: Vec<&str> = answer["notices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|notice| notice["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["ntc_incident", "ntc_maint"], "{answer}");
        let degraded = answer["degraded"].as_array().unwrap();
        assert_eq!(degraded.len(), 1, "{answer}");
        assert!(
            degraded[0].as_str().unwrap().contains(DOWN_MACHINE),
            "{answer}"
        );

        let calls = log.calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        for (machine, tool, arguments) in &calls {
            assert_ne!(machine, REPLICA_MACHINE);
            assert_eq!(tool, "orbit.notice.list");
            assert_eq!(arguments["all"], true, "{arguments}");
            assert!(arguments.get("limit").is_none(), "{arguments}");
            assert!(
                arguments["workspace"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("ws_")),
                "{arguments}"
            );
        }
    }

    /// The workspace-less route applies the shared default after collecting
    /// the owner's unbounded result.
    #[test]
    fn workspaceless_notice_list_uses_the_shared_default_limit() {
        let notices: Vec<Value> = (0..51)
            .map(|index| notice(&format!("ntc_{index:03}"), "info", "2026-11-02T07:00:00Z"))
            .collect();
        let probe = ScriptedProbe::new()
            .answering(
                OWNER_MACHINE,
                snapshot(
                    OWNER_MACHINE,
                    vec![workspace("ws_orbit", Some(OWNER_MACHINE))],
                ),
            )
            .on_call(
                OWNER_MACHINE,
                "orbit.notice.list",
                ScriptedToolResult::Answer(json!({"notices": notices, "degraded": []})),
            );
        let host = FederatedMcpHost::new(
            vec![destination("orbit-owner", OWNER_MACHINE)],
            Arc::new(probe),
        );
        let answer = host
            .call_tool(
                "orbit.notice.list",
                json!({}),
                ToolSessionContext::default(),
            )
            .unwrap();
        let ids: Vec<&str> = answer["notices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|notice| notice["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), 50);
        assert_eq!(ids.first(), Some(&"ntc_000"));
        assert_eq!(ids.last(), Some(&"ntc_049"));
    }

    /// An out-of-range `limit` on the workspace-less route is the same
    /// notice `invalid_field` refusal, with the same message, the local list
    /// gives, and no owner is read. ORB-24017: it was a generic
    /// `invalid_input` here.
    #[test]
    fn workspaceless_notice_list_refuses_a_bad_limit_like_the_local_list() {
        for limit in [json!(0), json!(500)] {
            let (host, log) = mux();
            let error = host
                .call_tool(
                    "orbit.notice.list",
                    json!({ "limit": limit }),
                    ToolSessionContext::default(),
                )
                .expect_err("an out-of-range limit is refused");
            assert!(
                matches!(
                    error,
                    OrbitError::NoticeRefused {
                        code: orbit_common::NoticeRefusalCode::InvalidField,
                        ..
                    }
                ),
                "{error:?}"
            );
            assert_eq!(
                error.to_string(),
                "notice_invalid_field: `limit` must be between 1 and 200"
            );
            assert!(log.calls().is_empty());
        }
    }

    /// Merged notices keep the shared list order across owners: kind, then
    /// newest `updated_at`, then id for a tie.
    #[test]
    fn workspaceless_notice_list_orders_ties_like_the_local_list() {
        let probe = ScriptedProbe::new()
            .answering(
                OWNER_MACHINE,
                snapshot(
                    OWNER_MACHINE,
                    vec![workspace("ws_orbit", Some(OWNER_MACHINE))],
                ),
            )
            .answering(
                OTHER_MACHINE,
                snapshot(
                    OTHER_MACHINE,
                    vec![workspace("ws_other", Some(OTHER_MACHINE))],
                ),
            )
            .on_call(
                OWNER_MACHINE,
                "orbit.notice.list",
                ScriptedToolResult::Answer(json!({
                    "notices": [
                        notice("ntc_b", "incident", "2026-11-02T07:00:00Z"),
                        notice("ntc_old", "incident", "2026-11-02T06:00:00Z"),
                    ],
                    "degraded": [],
                })),
            )
            .on_call(
                OTHER_MACHINE,
                "orbit.notice.list",
                ScriptedToolResult::Answer(json!({
                    "notices": [
                        notice("ntc_info", "info", "2026-11-02T09:00:00Z"),
                        notice("ntc_a", "incident", "2026-11-02T07:00:00+00:00"),
                    ],
                    "degraded": [],
                })),
            );
        let host = FederatedMcpHost::new(
            vec![
                destination("orbit-owner", OWNER_MACHINE),
                destination("orbit-other", OTHER_MACHINE),
            ],
            Arc::new(probe),
        );
        let answer = host
            .call_tool(
                "orbit.notice.list",
                json!({}),
                ToolSessionContext::default(),
            )
            .unwrap();
        let ids: Vec<&str> = answer["notices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|notice| notice["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["ntc_a", "ntc_b", "ntc_old", "ntc_info"], "{answer}");
    }

    /// A future kind or unreadable timestamp retains the previous federated
    /// fallback order: unknown kinds sort last, with readable update times
    /// before unreadable ones and id breaking ties.
    #[test]
    fn workspaceless_notice_list_preserves_fallback_order_for_unknown_values() {
        let probe = ScriptedProbe::new()
            .answering(
                OWNER_MACHINE,
                snapshot(
                    OWNER_MACHINE,
                    vec![workspace("ws_orbit", Some(OWNER_MACHINE))],
                ),
            )
            .on_call(
                OWNER_MACHINE,
                "orbit.notice.list",
                ScriptedToolResult::Answer(json!({
                    "notices": [
                        notice("ntc_zz_old", "future_kind", "2026-11-02T06:00:00Z"),
                        notice("ntc_known", "incident", "2026-11-02T09:00:00Z"),
                        notice("ntc_aa_new", "future_kind", "2026-11-02T07:00:00Z"),
                        notice("ntc_mm_unreadable", "future_kind", "not-a-timestamp"),
                    ],
                    "degraded": [],
                })),
            );
        let host = FederatedMcpHost::new(
            vec![destination("orbit-owner", OWNER_MACHINE)],
            Arc::new(probe),
        );
        let answer = host
            .call_tool(
                "orbit.notice.list",
                json!({}),
                ToolSessionContext::default(),
            )
            .unwrap();
        let ids: Vec<&str> = answer["notices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|notice| notice["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            ["ntc_known", "ntc_aa_new", "ntc_zz_old", "ntc_mm_unreadable"],
            "{answer}"
        );
    }

    /// With a selector the same tool is one routed workspace read, not an
    /// aggregation: the replica destination gets the call it was addressed
    /// (its own core then refuses the class it does not hold).
    #[test]
    fn notice_list_with_a_selector_routes_to_one_workspace() {
        let (host, log) = mux();
        let answer = host
            .call_tool(
                "orbit.notice.list",
                json!({ "workspace": format!("{OWNER_MACHINE}/ws_orbit") }),
                ToolSessionContext::default(),
            )
            .unwrap();
        assert_eq!(answer["notices"].as_array().unwrap().len(), 2);
        assert_eq!(log.calls().len(), 1);
    }
}

use std::sync::Arc;

use orbit_common::{HostRegistryCode, OrbitError};
use orbit_mcp::McpHost;
use orbit_mcp::federated::{Destination, DestinationSnapshot, FederatedMcpHost};
use orbit_registry::hosts::{HostEntry, TaskPrefixTable};
use orbit_types::tool::ToolSessionContext;
use serde_json::{Value, json};

use crate::scripted_destination::{
    OWNER_MACHINE, REPLICA_MACHINE, ScriptedProbe, ScriptedToolResult, destination, workspace,
};

#[test]
fn workspace_discovery_redacts_destination_envelopes() {
    use orbit_mcp::{HostFacts, execute_discovery_tool, execute_federated_workspace_discovery};
    use orbit_types::workspace::{WorkspaceCheckout, WorkspaceRegistry};

    let mut legacy = workspace("ws_orbit", Some(OWNER_MACHINE));
    legacy.git_remote = Some("https://operator:fixture-secret@github.com/example/orbit.git".into());
    let absent = workspace("ws_no_remote", None);
    let registry = WorkspaceRegistry {
        workspaces: vec![legacy.clone(), absent.clone()],
        checkouts: ["ws_orbit", "ws_no_remote"]
            .into_iter()
            .map(|id| WorkspaceCheckout::owner(id.into(), "/repo".into(), "/repo/.orbit".into()))
            .collect(),
        ..Default::default()
    };
    let facts = HostFacts {
        machine_id: OWNER_MACHINE.into(),
        ..Default::default()
    };
    let mut expected = json!([legacy, absent]);
    expected[0]["git_remote"] = json!("https://***@github.com/example/orbit.git");
    let local = execute_discovery_tool("orbit.workspace.list", &registry, &facts).unwrap();
    let destination_envelope = execute_federated_workspace_discovery(&registry, &facts);
    assert_eq!(local["workspaces"], expected);
    assert_eq!(destination_envelope["workspaces"], expected);

    assert!(
        registry.workspaces[0]
            .git_remote
            .as_deref()
            .unwrap()
            .contains("fixture-secret"),
        "projection must not mutate stored values"
    );
}

#[test]
fn workspace_discovery_redacts_older_peer_descriptors() {
    let mut legacy = workspace("ws_orbit", Some(OWNER_MACHINE));
    legacy.git_remote = Some("https://operator:fixture-secret@github.com/example/orbit.git".into());
    let absent = workspace("ws_no_remote", None);
    let mut expected = json!([legacy, absent]);
    expected[0]["git_remote"] = json!("https://***@github.com/example/orbit.git");
    // An older peer sends its unredacted Workspace. The accepting mux must
    // sanitize it independently rather than trusting the destination version.
    let probe = ScriptedProbe::new().answering(
        OWNER_MACHINE,
        DestinationSnapshot {
            machine_id: OWNER_MACHINE.into(),
            workspaces: vec![legacy, absent],
            crews: Default::default(),
            host: Default::default(),
        },
    );
    let mux = FederatedMcpHost::new(
        vec![destination("orbit-owner", OWNER_MACHINE)],
        Arc::new(probe),
    );
    let listed = mux
        .call_tool(
            "orbit.workspace.list",
            json!({}),
            ToolSessionContext::default(),
        )
        .unwrap();
    let rows = listed["workspaces"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for expected in expected.as_array().unwrap() {
        let actual = rows.iter().find(|row| row["id"] == expected["id"]).unwrap();
        let mut workspace_fields = actual.as_object().unwrap().clone();
        for key in [
            "selector",
            "machine_name",
            "machine_id",
            "reachability",
            "checkout_health",
            "capabilities",
        ] {
            workspace_fields.remove(key);
        }
        assert_eq!(Value::Object(workspace_fields), *expected);
    }
}
