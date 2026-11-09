//! The attention fan-out on the federated mux: authority per host, unreadable
//! destinations as items, the outer deadline, and one cursor across hosts.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_mcp::McpHost;
use orbit_mcp::federated::{
    AttentionFanout, Destination, DestinationProbe, DestinationSnapshot, FederatedMcpHost,
    RoutedSession,
};
use orbit_types::tool::{ToolSessionContext, mcp_advertised_tool_name};
use serde_json::{Value, json};

use crate::scripted_destination::{OWNER_MACHINE, workspace};

const FOLLOWER_MACHINE: &str = "hm_follower";

#[derive(Clone)]
enum Behaviour {
    Answer(Value),
    SelectorRefusal { remote: bool },
    Down,
    AnswersAs(&'static str),
    WithoutAttention,
    Slow(Duration),
}

/// Destinations whose behaviour a test changes between polls.
#[derive(Clone, Default)]
struct Fleet {
    behaviours: Arc<Mutex<HashMap<String, Behaviour>>>,
    hops: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Fleet {
    fn set(&self, machine_id: &str, behaviour: Behaviour) {
        self.behaviours
            .lock()
            .unwrap()
            .insert(machine_id.to_string(), behaviour);
    }
}

fn snapshot(machine_id: &str) -> DestinationSnapshot {
    DestinationSnapshot {
        machine_id: machine_id.to_string(),
        // Both hosts list the workspace the owner owns.
        workspaces: vec![workspace("ws_orbit", Some(OWNER_MACHINE))],
        ..Default::default()
    }
}

impl DestinationProbe for Fleet {
    fn probe(&self, destination: &Destination) -> Result<DestinationSnapshot, OrbitError> {
        Ok(snapshot(&destination.machine_id))
    }

    fn open_route(&self, destination: &Destination) -> Result<Box<dyn RoutedSession>, OrbitError> {
        let behaviour = self.behaviours.lock().unwrap()[&destination.machine_id].clone();
        match behaviour {
            Behaviour::Down => Err(OrbitError::UnreachableDestination(format!(
                "{}: ssh: connect to host owner port 22: Connection refused\nsecond line",
                destination.machine_id
            ))),
            Behaviour::Slow(delay) => {
                std::thread::sleep(delay);
                Err(OrbitError::UnreachableDestination("late".into()))
            }
            behaviour => Ok(Box::new(Route {
                machine_id: destination.machine_id.clone(),
                behaviour,
                hops: Arc::clone(&self.hops),
            })),
        }
    }
}

struct Route {
    machine_id: String,
    behaviour: Behaviour,
    hops: Arc<Mutex<Vec<(String, Value)>>>,
}

impl RoutedSession for Route {
    fn snapshot(&mut self) -> Result<DestinationSnapshot, OrbitError> {
        let answering = match self.behaviour {
            Behaviour::AnswersAs(other) => other.to_string(),
            _ => self.machine_id.clone(),
        };
        Ok(snapshot(&answering))
    }

    fn advertised_tools(&mut self) -> Result<Vec<String>, OrbitError> {
        let mut tools = vec!["orbit_workspace_list".to_string()];
        if !matches!(self.behaviour, Behaviour::WithoutAttention) {
            tools.push(mcp_advertised_tool_name("orbit.attention"));
        }
        Ok(tools)
    }

    fn supports_tool_argument(&mut self, name: &str, argument: &str) -> Result<bool, OrbitError> {
        Ok(name == "orbit.attention" && argument == "candidate_read")
    }

    fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
        session_context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        assert_eq!(name, "orbit.attention");
        assert_eq!(
            session_context.mcp_client_name.as_deref(),
            Some("orbit-federated-mux"),
            "a hop must announce the aggregator so the destination honours candidate_read"
        );
        self.hops
            .lock()
            .unwrap()
            .push((self.machine_id.clone(), arguments));
        match &self.behaviour {
            Behaviour::Answer(body) => Ok(body.clone()),
            Behaviour::SelectorRefusal { remote } => {
                let message = "tool 'orbit.attention' requires an explicit workspace selector";
                if *remote {
                    Err(OrbitError::RemoteTool {
                        code: "invalid_input".into(),
                        message: message.into(),
                        payload: json!({"code": "invalid_input", "message": message}),
                    })
                } else {
                    Err(OrbitError::InvalidInput(message.into()))
                }
            }
            _ => unreachable!("only answering destinations are called"),
        }
    }
}

fn item(kind: &str, machine_id: &str, task: &str, fingerprint: &str) -> Value {
    json!({
        "id": format!("att_{kind}_{task}"),
        "kind": kind,
        "severity": "medium",
        "workspace": {"id": "ws_orbit", "name": "orbit"},
        "host": {"machine_id": machine_id, "name": machine_id},
        "refs": {"task_id": task},
        "task": {"status": "proposed", "tags": [], "title": task},
        "since": "2026-11-01T00:00:00Z",
        "age_seconds": 3600,
        "reason": format!("{task} holds a warning"),
        "detail": {},
        "next": {"summary": "Re-pilot", "command": format!("orbit task show {task}")},
        "fingerprint": fingerprint,
    })
}

fn candidate_read(items: Vec<Value>) -> Value {
    json!({
        "schema_version": 1,
        "generated_at": "2026-11-02T00:00:00Z",
        "candidate_read": true,
        "filters": {"workspace": [{"id": "ws_orbit", "name": "orbit"}]},
        "counts": {"matching": items.len(), "returned": items.len(), "truncated": false,
                   "by_kind": {}, "by_severity": {}},
        "cursor": {"next": "atc1.discarded", "status": "none"},
        "items": items,
        "degraded": [],
        "message": "candidate items",
    })
}

fn mux(fleet: &Fleet, budget: Duration) -> FederatedMcpHost {
    FederatedMcpHost::new(
        vec![
            Destination::local(FOLLOWER_MACHINE, "mac"),
            Destination::ssh("owner.example", OWNER_MACHINE),
        ],
        Arc::new(fleet.clone()),
    )
    .with_attention_fanout(AttentionFanout {
        budget,
        host_names: BTreeMap::from([(OWNER_MACHINE.to_string(), "dk-server-2".to_string())]),
    })
}

fn attention(mux: &FederatedMcpHost, input: Value) -> Value {
    mux.call_tool("orbit_attention", input, ToolSessionContext::default())
        .expect("the fan-out answers even when a host does not")
}

fn kinds(response: &Value) -> Vec<&str> {
    response["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["kind"].as_str().unwrap())
        .collect()
}

/// A host that goes unreachable and comes back must not re-inject what the
/// caller already saw: the cursor minted while `degraded` was non-empty
/// carries the seen hashes forward (attention-cursor rule 4).
#[test]
fn an_owner_that_drops_out_and_returns_does_not_reinject_seen_items() {
    let fleet = Fleet::default();
    fleet.set(FOLLOWER_MACHINE, Behaviour::Answer(candidate_read(vec![])));
    let owner_items = candidate_read(vec![item("pilot_warning", OWNER_MACHINE, "ORB-1", "fp_1")]);
    fleet.set(OWNER_MACHINE, Behaviour::Answer(owner_items.clone()));
    let mux = mux(&fleet, Duration::from_secs(20));

    let first = attention(&mux, json!({"all": true}));
    assert_eq!(kinds(&first), ["pilot_warning"], "{first}");
    assert_eq!(first["items"][0]["host"]["machine_id"], OWNER_MACHINE);
    assert_eq!(
        first["items"][0]["workspace"]["selector"],
        format!("{OWNER_MACHINE}/ws_orbit"),
        "items carry the selector their host lists: {first}"
    );
    assert_eq!(first["hosts"].as_array().unwrap().len(), 2, "{first}");
    for (_, hop) in fleet.hops.lock().unwrap().iter() {
        assert_eq!(hop["candidate_read"], true, "{hop}");
        assert_eq!(hop["all"], true, "{hop}");
        assert!(
            hop.get("since").is_none() && hop.get("limit").is_none(),
            "{hop}"
        );
    }

    fleet.set(OWNER_MACHINE, Behaviour::Down);
    let blind = attention(&mux, json!({"all": true, "since": first["cursor"]["next"]}));
    assert_eq!(kinds(&blind), ["host_unhealthy"], "{blind}");
    let unhealthy = &blind["items"][0];
    assert_eq!(
        unhealthy["severity"], "critical",
        "the owner of an in-scope workspace: {blind}"
    );
    assert_eq!(unhealthy["detail"]["cause"], "unreachable_destination");
    assert_eq!(unhealthy["host"]["name"], "dk-server-2");
    assert_eq!(unhealthy["workspace"], json!({"id": null, "name": null}));
    assert!(
        unhealthy["reason"]
            .as_str()
            .unwrap()
            .starts_with("dk-server-2 unreachable: "),
        "{unhealthy}"
    );
    assert_eq!(
        blind["degraded"][0]["code"], "unreachable_destination",
        "{blind}"
    );

    // Excluding the kind hides the item, never the blind spot.
    let excluded = attention(
        &mux,
        json!({"all": true, "exclude_kind": "host_unhealthy", "since": first["cursor"]["next"]}),
    );
    assert_eq!(excluded["items"], json!([]), "{excluded}");
    assert_eq!(excluded["degraded"][0]["code"], "unreachable_destination");
    let message = excluded["message"].as_str().unwrap();
    assert!(
        message.starts_with("No items found; 1 sources could not be read"),
        "an empty list with a blind host is not all-clear: {message}"
    );

    fleet.set(OWNER_MACHINE, Behaviour::Answer(owner_items));
    let recovered = attention(&mux, json!({"all": true, "since": blind["cursor"]["next"]}));
    assert_eq!(
        recovered["items"],
        json!([]),
        "the returning owner re-injected an item already shown: {recovered}"
    );
    assert_eq!(recovered["counts"]["unchanged"], 1, "{recovered}");
}

/// A pinned identity mismatch and a binary without the tool are
/// `host_unhealthy` with the federated routing identity, and a slow host is
/// cut at the outer deadline instead of holding the call.
#[test]
fn unreadable_destinations_become_items_within_the_budget() {
    let fleet = Fleet::default();
    fleet.set(FOLLOWER_MACHINE, Behaviour::Answer(candidate_read(vec![])));

    for (behaviour, cause) in [
        (
            Behaviour::AnswersAs("hm_impostor"),
            "unreachable_destination",
        ),
        (Behaviour::WithoutAttention, "tool_not_on_this_host"),
    ] {
        fleet.set(OWNER_MACHINE, behaviour);
        let response = attention(&mux(&fleet, Duration::from_secs(20)), json!({}));
        assert_eq!(kinds(&response), ["host_unhealthy"], "{response}");
        assert_eq!(response["items"][0]["detail"]["cause"], cause, "{response}");
        assert_eq!(response["degraded"][0]["code"], cause, "{response}");
    }

    fleet.set(OWNER_MACHINE, Behaviour::Slow(Duration::from_secs(5)));
    let started = Instant::now();
    let response = attention(&mux(&fleet, Duration::from_millis(300)), json!({}));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "one slow host held the call for {:?}",
        started.elapsed()
    );
    assert_eq!(kinds(&response), ["host_unhealthy"], "{response}");
    assert_eq!(
        response["items"][0]["detail"]["cause"],
        "unreachable_destination"
    );
}

#[test]
fn selector_refusals_are_distinct_from_unreachable_attention_destinations() {
    let fleet = Fleet::default();
    fleet.set(FOLLOWER_MACHINE, Behaviour::Answer(candidate_read(vec![])));
    for remote in [false, true] {
        fleet.set(OWNER_MACHINE, Behaviour::SelectorRefusal { remote });
        let response = attention(&mux(&fleet, Duration::from_secs(20)), json!({}));
        assert_eq!(
            response["hosts"][0]["reachability"], "reachable",
            "{response}"
        );
        assert_eq!(
            response["hosts"][1]["reachability"], "reachable",
            "{response}"
        );
        assert_eq!(kinds(&response), ["host_unhealthy"], "{response}");
        assert_eq!(
            response["items"][0]["detail"]["cause"], "attention_read_refused",
            "{response}"
        );
        assert_eq!(
            response["degraded"][0]["code"], "attention_read_refused",
            "{response}"
        );
        let reason = response["items"][0]["reason"].as_str().unwrap();
        assert!(reason.contains("attention read refused"), "{response}");
        assert!(reason.contains("workspace selector"), "{response}");
        assert!(!reason.contains("unreachable"), "{response}");
    }
}

fn session_on_follower() -> ToolSessionContext {
    ToolSessionContext {
        workspace: Some(format!("{FOLLOWER_MACHINE}/ws_orbit")),
        ..ToolSessionContext::default()
    }
}

fn attention_as(mux: &FederatedMcpHost, input: Value, session: ToolSessionContext) -> Value {
    mux.call_tool("orbit_attention", input, session)
        .expect("the fan-out answers even when a host does not")
}

/// The 2026-11-08 federated read: a session bound to the follower's checkout,
/// `host` plus `all`, and a workspace filter, all came back empty with nothing
/// degraded. The owner must be read, and the workspace filter must survive.
#[test]
fn a_bound_session_host_filter_reads_the_owner() {
    let fleet = Fleet::default();
    fleet.set(FOLLOWER_MACHINE, Behaviour::Answer(candidate_read(vec![])));
    let warning = item("pilot_warning", OWNER_MACHINE, "ORB-23322", "fp_warn");
    fleet.set(
        OWNER_MACHINE,
        Behaviour::Answer(candidate_read(vec![warning])),
    );
    let mux = mux(&fleet, Duration::from_secs(20));
    let session = session_on_follower();

    let response = attention_as(
        &mux,
        json!({
            "host": "dk-server-2",
            "kind": ["pilot_warning", "pilot_hold"],
            "all": true,
        }),
        session.clone(),
    );
    assert!(
        !(response["items"].as_array().unwrap().is_empty()
            && response["degraded"].as_array().unwrap().is_empty()),
        "an unread owner must not look all-clear: {response}"
    );
    assert_eq!(kinds(&response), ["pilot_warning"], "{response}");
    assert_eq!(response["items"][0]["host"]["machine_id"], OWNER_MACHINE);
    let hops = fleet.hops.lock().unwrap().clone();
    assert!(
        hops.iter().any(|(machine, hop)| {
            machine == OWNER_MACHINE && hop["candidate_read"] == true && hop["all"] == true
        }),
        "the owner was not read: {hops:?}"
    );

    fleet.hops.lock().unwrap().clear();
    let mut silent = candidate_read(vec![]);
    silent["filters"] = json!({"workspace": []});
    fleet.set(OWNER_MACHINE, Behaviour::Answer(silent));
    let filtered = attention_as(
        &mux,
        json!({"workspace": "ws_orbit", "host": "dk-server-2"}),
        session.clone(),
    );
    let echoed = filtered["filters"]["workspace"].as_array().unwrap();
    assert!(
        echoed
            .iter()
            .any(|workspace| { workspace["id"] == "ws_orbit" || workspace["name"] == "orbit" }),
        "the workspace filter was dropped: {filtered}"
    );
    let message = filtered["message"].as_str().unwrap();
    assert!(
        message.contains("dk-server-2") && message.contains("orbit"),
        "an empty result must name the hosts and workspaces read: {message}"
    );
    assert!(
        !(filtered["items"].as_array().unwrap().is_empty()
            && filtered["degraded"].as_array().unwrap().is_empty()
            && !message.contains("dk-server-2")),
        "{filtered}"
    );

    fleet.hops.lock().unwrap().clear();
    fleet.set(
        OWNER_MACHINE,
        Behaviour::Answer(candidate_read(vec![item(
            "pilot_warning",
            OWNER_MACHINE,
            "ORB-23322",
            "fp_warn",
        )])),
    );
    let qualified = attention_as(
        &mux,
        json!({
            "workspace": format!("{FOLLOWER_MACHINE}/ws_orbit"),
            "host": "dk-server-2",
        }),
        session,
    );
    assert_eq!(kinds(&qualified), ["pilot_warning"], "{qualified}");
    let hops = fleet.hops.lock().unwrap().clone();
    assert!(
        hops.iter().any(|(machine, hop)| {
            machine == OWNER_MACHINE
                && hop["workspace"] == json!(["ws_orbit"])
                && hop["candidate_read"] == true
        }),
        "a replica selector must still read the owner: {hops:?}"
    );
}

/// A workspace whose owner is not among the destinations is a blind spot, not
/// an empty page. The host filter is what kept the owner out.
#[test]
fn an_unread_owner_of_a_named_workspace_is_degraded() {
    let fleet = Fleet::default();
    fleet.set(FOLLOWER_MACHINE, Behaviour::Answer(candidate_read(vec![])));
    let mux = FederatedMcpHost::new(
        vec![Destination::local(FOLLOWER_MACHINE, "mac")],
        Arc::new(fleet.clone()),
    )
    .with_attention_fanout(AttentionFanout {
        budget: Duration::from_secs(20),
        host_names: BTreeMap::from([(OWNER_MACHINE.to_string(), "dk-server-2".to_string())]),
    });
    let response = attention(
        &mux,
        json!({"workspace": "ws_orbit", "kind": ["pilot_warning"]}),
    );
    assert!(
        response["items"].as_array().unwrap().is_empty(),
        "{response}"
    );
    let degraded = response["degraded"].as_array().unwrap();
    assert!(
        degraded.iter().any(|entry| {
            entry["code"] == "source_unavailable"
                && entry["scope"] == "host:dk-server-2"
                && entry["message"]
                    .as_str()
                    .unwrap()
                    .contains("owns workspace 'orbit'")
        }),
        "the unread owner is missing from degraded: {response}"
    );
    let message = response["message"].as_str().unwrap();
    assert!(
        message.starts_with("No items found; ") && message.contains("could not be read"),
        "an empty list with an unread owner is not all-clear: {message}"
    );
}
