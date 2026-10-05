use std::sync::{Arc, Mutex};

use orbit_agent::loop_engine::tool_dispatch::build_tool_specs;
use orbit_agent::loop_engine::{
    AgentLoopConfig, AgentLoopError, ContentBlock, InMemorySink, LoopAuditEvent, Session,
};
use orbit_agent::providers::openai_compat::OpenAiCompatTransport;
use orbit_common::OrbitError;
use orbit_tools::{
    OrbitBuiltinAction, OrbitTaskScope, OrbitToolHost, ReservationOwnerContext, ToolContext,
    ToolRegistry,
};
use orbit_types::workflow::activity_job::{OnDenial, tool_allowed};
use serde_json::{Value, json};

use super::support::{Server, WAIT, isolated, scratch};

#[derive(Default)]
struct RecordingHost(Mutex<Vec<OrbitBuiltinAction>>);

impl OrbitToolHost for RecordingHost {
    fn execute(
        &self,
        action: OrbitBuiltinAction,
        input: Value,
        _agent: Option<String>,
        _model: Option<String>,
        _reservation_owner: Option<ReservationOwnerContext>,
    ) -> Result<Value, OrbitError> {
        self.0.lock().unwrap().push(action);
        Ok(input)
    }

    fn task_scope(&self) -> OrbitTaskScope {
        OrbitTaskScope {
            orbit_root: None,
            task_id: None,
            run_id: None,
        }
    }
}

#[test]
fn dispatch_requires_an_active_advertised_tool_and_execution_grant() {
    isolated(
        "loop_policy::dispatch_requires_an_active_advertised_tool_and_execution_grant",
        || {
            let mut registry = ToolRegistry::new();
            registry.register_builtins();
            assert!(registry.has("orbit.task.delete"));
            assert!(!registry.is_active("orbit.task.delete"));

            // Expected dispatch outcomes guard the inactive-tool incident and
            // the intersection of actual advertisement and execution policy.
            type DispatchCase<'a> = (&'a [&'a str], Option<&'a [&'a str]>, &'a str, bool);
            let cases: &[DispatchCase<'_>] = &[
                (&["orbit.task.*"], None, "orbit.task.delete", false),
                (&["*"], None, "orbit.task.delete", false),
                (&["orbit.task.delete"], None, "orbit.task.delete", false),
                (&["orbit.task.*"], None, "orbit.task.show", true),
                (&["*"], None, "orbit.task.show", false),
                (&["orbit.*"], None, "orbit.task.show", false),
                (&["orbit.task.*"], None, "orbit.task.unknown", false),
                (&["orbit.task.unknown"], None, "orbit.task.unknown", false),
                (&[], None, "orbit.task.show", false),
                (
                    &["orbit.task.*"],
                    Some(&["orbit.task.list"]),
                    "orbit.task.show",
                    false,
                ),
                (
                    &["orbit.task.show"],
                    Some(&["orbit.task.*"]),
                    "orbit.task.list",
                    false,
                ),
                (&["orbit.task.show"], Some(&[]), "orbit.task.show", false),
                (&["*"], Some(&["orbit.task.show"]), "orbit.task.show", true),
            ];
            for &(allowlist, advertised, tool_name, executes) in cases {
                for on_denial in [OnDenial::Terminate, OnDenial::Continue] {
                    let fixture = scratch();
                    let mut cfg = AgentLoopConfig::new_for_run("fixture-run")
                        .with_allowlist(allowlist.iter().map(|name| (*name).into()).collect())
                        .with_on_denial(on_denial);
                    if let Some(advertised) = advertised {
                        cfg = cfg.with_advertised_tools(
                            advertised.iter().map(|name| (*name).into()).collect(),
                        );
                    }
                    let specs = build_tool_specs(
                        &registry,
                        cfg.advertised_tools.as_ref().unwrap_or(&cfg.tool_allowlist),
                    );
                    let advertised_names: Vec<_> =
                        specs.iter().map(|spec| spec.name.as_str()).collect();
                    assert_eq!(
                        executes,
                        advertised_names.contains(&tool_name)
                            && tool_allowed(tool_name, &cfg.tool_allowlist),
                        "dispatch must match the advertised active specs and execution allowlist"
                    );

                    let input = if tool_name == "orbit.task.delete" {
                        json!({"id":"T-fixture","force":true})
                    } else {
                        json!({"id":"T-fixture"})
                    };
                    let mut responses = vec![json!({"choices":[{
                        "message":{"role":"assistant","tool_calls":[{
                            "id":"call-policy","type":"function","function":{
                                "name":tool_name,
                                "arguments":input.to_string()
                            }
                        }]},"finish_reason":"tool_calls"
                    }]})];
                    let terminates = !executes && matches!(on_denial, OnDenial::Terminate);
                    if !terminates {
                        responses.push(json!({"choices":[{
                            "message":{"role":"assistant","content":"done"},
                            "finish_reason":"stop"
                        }]}));
                    }
                    let server = Server::new(responses);
                    let transport = OpenAiCompatTransport::new(
                        &server.base_url,
                        "synthetic-api-key",
                        "fixture-model",
                        vec![],
                    )
                    .unwrap()
                    .with_timeout(WAIT)
                    .unwrap();
                    let host = Arc::new(RecordingHost::default());
                    let ctx = ToolContext {
                        allowed_tools: cfg.tool_allowlist.clone(),
                        orbit_host: Some(host.clone()),
                        ..Default::default()
                    };
                    let sink = InMemorySink::new(fixture.path().join("audit"));
                    let mut session = Session::new("openai_compat", "fixture-model", "", None);
                    let outcome = session.send(
                        &cfg,
                        &transport,
                        &registry,
                        &ctx,
                        &sink,
                        "check tool policy",
                    );
                    if terminates {
                        assert!(
                            matches!(outcome, Err(AgentLoopError::PolicyDenied {
                            tool_name: ref denied, iteration: 1,
                        }) if denied == tool_name),
                            "expected policy denial: {outcome:?}"
                        );
                    } else {
                        let outcome = outcome.expect("allowed calls and continued denials finish");
                        assert_eq!(
                            outcome.trace[0].policy_denials,
                            if executes {
                                vec![]
                            } else {
                                vec![tool_name.to_string()]
                            }
                        );
                    }
                    assert_eq!(
                        *host.0.lock().unwrap(),
                        if executes {
                            vec![OrbitBuiltinAction::TaskShow]
                        } else {
                            vec![]
                        },
                        "inactive or unadvertised calls must have zero executions"
                    );
                    let denials: Vec<_> = sink
                        .events()
                        .into_iter()
                        .filter_map(|event| match event {
                            LoopAuditEvent::PolicyDenial { tool_name, .. } => Some(tool_name),
                            _ => None,
                        })
                        .collect();
                    assert_eq!(
                        denials,
                        if executes {
                            vec![]
                        } else {
                            vec![tool_name.to_string()]
                        }
                    );
                    let results: Vec<_> = session
                        .history()
                        .iter()
                        .flat_map(|message| &message.content)
                        .filter_map(|block| match block {
                            ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                            } => Some((
                                tool_use_id,
                                serde_json::from_str::<Value>(content).unwrap(),
                                is_error,
                            )),
                            _ => None,
                        })
                        .collect();
                    assert_eq!(results.len(), 1, "every tool_use must receive a result");
                    assert_eq!(results[0].0, "call-policy");
                    assert_eq!(*results[0].2, !executes);
                    if !executes {
                        assert_eq!(results[0].1["error"]["code"], "tool_denied");
                    }

                    let request = server.request();
                    let wire_names: Vec<_> = request.body["tools"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|tool| tool["function"]["name"].as_str().unwrap())
                        .collect();
                    assert_eq!(wire_names, advertised_names);
                    if !terminates {
                        server.request();
                    }
                    server.finish();
                }
            }
        },
    );
}
