//! `orbit.attention` fan-out read of the federated mux.
//!
//! A call that names `host`, `all`, or a workspace — including a
//! host-qualified selector — reads every in-scope destination (the implicit
//! local one plus each host in the operator's host file, narrowed by
//! `host`). A session binding is the default only when the call names none
//! of those; it must not turn an explicit host or workspace into a
//! local-only read. The hop carries `all: true` or the workspaces that
//! destination itself lists, the item filters and `candidate_read: true`,
//! never `since` or `limit`. Each destination reports only what it is
//! authoritative for. A destination that cannot be read, and an owner of an
//! in-scope workspace who was not a destination of this read, becomes a
//! `degraded` entry, never a shorter all-clear list. Ordering, the cursor
//! and the public limit then run once, here. Nothing is written and nothing
//! is cached between calls.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_common::governance::attention::{
    ATTENTION_AGGREGATOR_CLIENT, attention_candidate_read_refusal, cursor,
    filter::{AttentionFilter, attention_strings},
    page::{AttentionPageCursor, AttentionPageInput, attention_public_page},
};
use orbit_common::{OrbitError, RemoteToolCode};
use orbit_types::tool::{
    AttentionDegraded, AttentionEnvelope, AttentionHost, AttentionHostStatus, AttentionItem,
    AttentionKind, AttentionNext, AttentionSeverity, AttentionWorkspace, ToolSessionContext,
    mcp_advertised_tool_name,
};
use serde_json::{Value, json};

use super::config::{Destination, MachineQualifiedSelector};
use super::descriptor::WorkspaceDescriptor;
use super::host::{FederatedMcpHost, confirm_pinned_identity, tool_on_surface};
use super::probe::{DestinationProbe, DestinationSnapshot};

const ATTENTION_TOOL: &str = "orbit.attention";
/// Routing identity of a destination that did not answer, timed out, or
/// answered under another `machine_id` than its pin.
const UNREACHABLE: &str = "unreachable_destination";
/// Routing identity of a reachable destination whose binary lacks the tool.
const TOOL_MISSING: &str = "tool_not_on_this_host";
/// A destination answered, but refused the read's input (including its scope).
const READ_REFUSED: &str = "attention_read_refused";

/// Seconds the cross-host attention fan-out waits for every hop.
///
/// The former `attention.fanout_budget_seconds` default, fixed when the key was removed [ORB-22808].
pub const ATTENTION_FANOUT_BUDGET_SECONDS: u64 = 20;

/// What the fan-out needs from its caller.
#[derive(Debug, Clone)]
pub struct AttentionFanout {
    /// The outer deadline for every hop, [`ATTENTION_FANOUT_BUDGET_SECONDS`]
    /// unless a fixture shortens it.
    pub budget: Duration,
    /// Registered host names by `machine_id`, for attribution and `--host`.
    pub host_names: BTreeMap<String, String>,
}

impl Default for AttentionFanout {
    fn default() -> Self {
        Self {
            budget: Duration::from_secs(ATTENTION_FANOUT_BUDGET_SECONDS),
            host_names: BTreeMap::new(),
        }
    }
}

/// One destination's outcome.
enum HopOutcome {
    /// The destination answered the read.
    Answered {
        snapshot: DestinationSnapshot,
        body: Value,
    },
    /// The destination lists none of the named workspaces.
    OutOfScope { snapshot: DestinationSnapshot },
    /// The destination could not be read; `cause` is its routing identity.
    Failed {
        cause: &'static str,
        message: String,
        snapshot: Option<DestinationSnapshot>,
    },
}

impl FederatedMcpHost {
    /// Answer `orbit.attention` by fanning out, or `None` when the call is
    /// routed like any other: another tool, a worker-bound call, or a bare
    /// call whose only scope is a host-qualified session binding.
    ///
    /// `host`, `all`, and a workspace argument are fleet scope. Returning
    /// `None` for those used to route the call to the bound checkout with
    /// the host filter still attached, and that local read then dropped
    /// every other host and reported an empty page with nothing degraded
    /// [ORB-23333].
    pub(super) fn aggregate_attention(
        &self,
        name: &str,
        input: &Value,
        session_context: &ToolSessionContext,
    ) -> Option<Result<Value, OrbitError>> {
        if mcp_advertised_tool_name(name) != mcp_advertised_tool_name(ATTENTION_TOOL)
            || session_context.worker_invocation.is_some()
        {
            return None;
        }
        // A user's value is never copied onto a hop: the mux sets it itself.
        if input.get("candidate_read").is_some() {
            return Some(Err(attention_candidate_read_refusal()));
        }
        let named_scope = input.get("all") == Some(&Value::Bool(true))
            || !attention_strings(input, "host").is_empty()
            || !attention_strings(input, "workspace").is_empty();
        let session_routes_one_host = !named_scope
            && session_context
                .workspace
                .as_deref()
                .is_some_and(qualified_selector);
        if session_routes_one_host {
            return None;
        }
        Some(self.fan_out_attention(input, session_context))
    }

    fn fan_out_attention(
        &self,
        input: &Value,
        session_context: &ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let filter = AttentionFilter::parse(input.clone())?;
        let now = Utc::now();
        let settings = self.attention_fanout();
        let destinations = self.attention_destinations(&attention_strings(input, "host"))?;
        let selectors = attention_strings(input, "workspace");
        let mut hop = input.clone();
        if let Some(object) = hop.as_object_mut() {
            for key in ["since", "limit", "host", "workspace", "all"] {
                object.remove(key);
            }
            object.insert("candidate_read".into(), Value::Bool(true));
        }
        let mut context = session_context.clone();
        context.mcp_client_name = Some(ATTENTION_AGGREGATOR_CLIENT.to_string());
        let outcomes = fan_out(
            self.probe(),
            &destinations,
            &hop,
            &selectors,
            &context,
            settings.budget,
        );

        // Owners of in-scope workspaces, as the answering hosts' checkouts name them.
        let owners = outcomes
            .iter()
            .filter_map(|outcome| match outcome {
                HopOutcome::Answered { snapshot, .. } => Some(snapshot),
                _ => None,
            })
            .flat_map(|snapshot| {
                snapshot
                    .workspaces
                    .iter()
                    .filter(|workspace| in_scope(&selectors, &snapshot.machine_id, workspace))
                    .filter_map(|workspace| workspace.owner_machine_id.clone())
            })
            .collect::<BTreeSet<_>>();
        let local_facts = destinations
            .iter()
            .zip(&outcomes)
            .find_map(|(destination, outcome)| match outcome {
                HopOutcome::Answered { snapshot, .. } | HopOutcome::OutOfScope { snapshot }
                    if destination.is_local() =>
                {
                    Some(snapshot.host.clone())
                }
                _ => None,
            });

        let attempted = destinations
            .iter()
            .map(|destination| destination.machine_id.clone())
            .collect::<BTreeSet<_>>();
        let mut items = Vec::new();
        let mut degraded = Vec::new();
        let mut hosts = Vec::new();
        let mut workspaces = Vec::new();
        let mut read_workspaces = Vec::new();
        let mut matched_selectors = BTreeSet::new();
        let mut noted_owners = BTreeSet::new();
        let mut every_destination_answered = true;
        for (destination, outcome) in destinations.iter().zip(outcomes) {
            let host = AttentionHost {
                machine_id: destination.machine_id.clone(),
                name: settings.host_name(destination, outcome_snapshot(&outcome)),
            };
            let owns = owners.contains(&destination.machine_id);
            match outcome {
                HopOutcome::Failed {
                    cause,
                    message,
                    snapshot,
                } => {
                    every_destination_answered = false;
                    hosts.push(host_status(
                        &host,
                        if cause == READ_REFUSED {
                            "reachable"
                        } else {
                            cause
                        },
                    ));
                    degraded.push(AttentionDegraded {
                        scope: format!("host:{}", host.name),
                        kind: AttentionKind::HostUnhealthy.as_str().into(),
                        code: cause.into(),
                        message: message.clone(),
                    });
                    items.push(unhealthy_item(&host, cause, &message, owns));
                    if let Some(snapshot) = snapshot.as_ref() {
                        note_unread_owners(
                            snapshot,
                            &selectors,
                            &attempted,
                            &settings.host_names,
                            &mut noted_owners,
                            &mut degraded,
                        );
                    }
                }
                HopOutcome::OutOfScope { snapshot } => {
                    hosts.push(host_status(&host, "reachable"));
                    matched_selectors.extend(matched(&selectors, &snapshot));
                    items.extend(skew_item(&host, &snapshot, local_facts.as_ref(), owns));
                    note_unread_owners(
                        &snapshot,
                        &selectors,
                        &attempted,
                        &settings.host_names,
                        &mut noted_owners,
                        &mut degraded,
                    );
                }
                HopOutcome::Answered { snapshot, body } => {
                    hosts.push(host_status(&host, "reachable"));
                    matched_selectors.extend(matched(&selectors, &snapshot));
                    items.extend(skew_item(&host, &snapshot, local_facts.as_ref(), owns));
                    for workspace in snapshot
                        .workspaces
                        .iter()
                        .filter(|workspace| in_scope(&selectors, &snapshot.machine_id, workspace))
                    {
                        remember_workspace(
                            &mut read_workspaces,
                            echoed_workspace(destination, workspace),
                        );
                    }
                    note_unread_owners(
                        &snapshot,
                        &selectors,
                        &attempted,
                        &settings.host_names,
                        &mut noted_owners,
                        &mut degraded,
                    );
                    let envelope = match serde_json::from_value::<AttentionEnvelope>(body) {
                        Ok(envelope) => envelope,
                        Err(error) => {
                            degraded.push(truncated_entry(
                                &host,
                                format!("unreadable attention response: {error}"),
                            ));
                            continue;
                        }
                    };
                    if !complete_candidate_set(&envelope) {
                        degraded.push(truncated_entry(
                            &host,
                            format!(
                                "{} reported {} matching items and returned {}; the rest are not \
                                 in this list",
                                host.name,
                                envelope.counts.matching,
                                envelope.items.len()
                            ),
                        ));
                    }
                    let selector_of = |workspace_id: &str| {
                        snapshot
                            .workspaces
                            .iter()
                            .find(|workspace| workspace.id == workspace_id)
                            .and_then(|workspace| {
                                WorkspaceDescriptor::reachable(destination, workspace.clone())
                                    .selector()
                                    .map(ToOwned::to_owned)
                            })
                    };
                    if let Some(listed) =
                        envelope.filters.get("workspace").and_then(Value::as_array)
                    {
                        for workspace in listed {
                            let mut workspace = workspace.clone();
                            if let Some(selector) = workspace["id"].as_str().and_then(&selector_of)
                            {
                                workspace["selector"] = json!(selector);
                            }
                            workspaces.push(workspace);
                        }
                    }
                    for mut item in envelope.items {
                        if !item.workspace.id.is_empty() {
                            item.workspace.selector = selector_of(&item.workspace.id);
                        }
                        items.push(item);
                    }
                    degraded.extend(envelope.degraded);
                }
            }
        }
        if workspaces.is_empty() {
            // A destination that answered without echoing its workspaces must
            // not wipe the scope this read actually opened [ORB-23333].
            workspaces = read_workspaces;
        }
        if every_destination_answered
            && let Some(unknown) = selectors
                .iter()
                .find(|selector| !matched_selectors.contains(*selector))
        {
            return Err(OrbitError::InvalidInput(format!(
                "unknown_workspace: no in-scope host lists workspace '{unknown}'"
            )));
        }
        // Aggregator-built items pass the same item filters the hops applied.
        items.retain(|item| item.kind != AttentionKind::HostUnhealthy || filter.matches(item));

        let mut echoed = input.clone();
        if let Some(object) = echoed.as_object_mut() {
            object.remove("since");
            object.remove("model");
            object.remove("agent");
        }
        echoed["workspace"] = Value::Array(workspaces);
        let envelope = attention_public_page(
            AttentionPageInput {
                filters: echoed,
                items,
                degraded,
                hosts,
            },
            AttentionPageCursor {
                since: input.get("since").and_then(Value::as_str),
                limit: filter.limit,
            },
            now,
        );
        serde_json::to_value(envelope).map_err(|error| {
            OrbitError::Execution(format!("serialize attention response: {error}"))
        })
    }

    /// The destinations a `host` filter keeps, in configured order. A filter
    /// that names no configured destination is an error, never an empty list.
    fn attention_destinations(&self, hosts: &[String]) -> Result<Vec<Destination>, OrbitError> {
        let settings = self.attention_fanout();
        let names = |destination: &Destination| -> Vec<String> {
            let mut names = vec![
                destination.machine_id.clone(),
                destination.machine_name_display().to_string(),
            ];
            names.extend(settings.host_names.get(&destination.machine_id).cloned());
            names
        };
        for selector in hosts {
            if !self.destinations().iter().any(|destination| {
                names(destination)
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(selector.trim()))
            }) {
                return Err(OrbitError::InvalidInput(format!(
                    "unknown_host: {selector}; run `orbit host list` for the registered hosts"
                )));
            }
        }
        Ok(self
            .destinations()
            .iter()
            .filter(|destination| {
                hosts.is_empty()
                    || hosts.iter().any(|selector| {
                        names(destination)
                            .iter()
                            .any(|name| name.eq_ignore_ascii_case(selector.trim()))
                    })
            })
            .cloned()
            .collect())
    }
}

impl AttentionFanout {
    /// The registered name, else the name the destination reported, else
    /// its configured display identity.
    fn host_name(
        &self,
        destination: &Destination,
        snapshot: Option<&DestinationSnapshot>,
    ) -> String {
        self.host_names
            .get(&destination.machine_id)
            .cloned()
            .or_else(|| snapshot.and_then(|snapshot| snapshot.host.machine_name.clone()))
            .unwrap_or_else(|| destination.machine_name_display().to_string())
    }
}

/// Run every hop on its own thread and wait at most `budget` for all of
/// them. A hop still running at the deadline is unreachable for this call;
/// its thread finishes on its own transport timeouts and its answer is
/// dropped, so one slow host never holds the others.
fn fan_out(
    probe: Arc<dyn DestinationProbe>,
    destinations: &[Destination],
    hop: &Value,
    selectors: &[String],
    context: &ToolSessionContext,
    budget: Duration,
) -> Vec<HopOutcome> {
    let deadline = Instant::now() + budget;
    // One slot per hop: each hop sends exactly once, so no send ever blocks,
    // even after the receiver gave up at the deadline.
    let (sender, receiver) = mpsc::sync_channel(destinations.len().max(1));
    for (index, destination) in destinations.iter().enumerate() {
        let hop_sender = sender.clone();
        let probe = Arc::clone(&probe);
        let destination = destination.clone();
        let hop = hop.clone();
        let selectors = selectors.to_vec();
        let context = context.clone();
        let spawned = std::thread::Builder::new()
            .name("attention-hop".into())
            .spawn(move || {
                let outcome =
                    read_destination(probe.as_ref(), &destination, hop, &selectors, context);
                // The receiver is gone once the deadline passed.
                let _ = hop_sender.send((index, outcome));
            });
        if let Err(error) = spawned {
            let _ = sender.send((
                index,
                HopOutcome::Failed {
                    cause: UNREACHABLE,
                    message: format!("could not start the read: {error}"),
                    snapshot: None,
                },
            ));
        }
    }
    drop(sender);
    let mut outcomes = (0..destinations.len()).map(|_| None).collect::<Vec<_>>();
    let mut pending = destinations.len();
    while pending > 0 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok((index, outcome)) => {
                if outcomes[index].is_none() {
                    pending -= 1;
                }
                outcomes[index] = Some(outcome);
            }
            Err(_) => break,
        }
    }
    outcomes
        .into_iter()
        .map(|outcome| {
            outcome.unwrap_or_else(|| HopOutcome::Failed {
                cause: UNREACHABLE,
                message: format!(
                    "did not answer within the {}s attention fan-out budget",
                    budget.as_secs_f64()
                ),
                snapshot: None,
            })
        })
        .collect()
}

/// One destination: identity against its pin, the workspaces it lists, the
/// tool and the `candidate_read` argument on its live surface, then the read.
fn read_destination(
    probe: &dyn DestinationProbe,
    destination: &Destination,
    mut hop: Value,
    selectors: &[String],
    context: ToolSessionContext,
) -> HopOutcome {
    let failed = |cause, error: &dyn std::fmt::Display, snapshot| HopOutcome::Failed {
        cause,
        message: error.to_string(),
        snapshot,
    };
    let mut session = match probe.open_worker_route(destination, &context) {
        Ok(session) => session,
        Err(error) => return failed(UNREACHABLE, &error, None),
    };
    let snapshot = match session.snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => return failed(UNREACHABLE, &error, None),
    };
    if let Err(error) = confirm_pinned_identity(destination, &snapshot) {
        return failed(UNREACHABLE, &error, None);
    }
    if !selectors.is_empty() {
        let ids = snapshot
            .workspaces
            .iter()
            .filter(|workspace| in_scope(selectors, &snapshot.machine_id, workspace))
            .map(|workspace| json!(workspace.id))
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return HopOutcome::OutOfScope { snapshot };
        }
        hop["workspace"] = Value::Array(ids);
    } else {
        hop["all"] = Value::Bool(true);
    }
    match session.advertised_tools() {
        Ok(tools) if tool_on_surface(&tools, ATTENTION_TOOL) => {}
        Ok(_) => {
            return failed(
                TOOL_MISSING,
                &format!(
                    "'{ATTENTION_TOOL}' is not advertised on '{}'",
                    destination.machine_id
                ),
                Some(snapshot),
            );
        }
        Err(error) => return failed(UNREACHABLE, &error, Some(snapshot)),
    }
    // A destination whose schema predates the candidate read gets the public
    // arguments; completeness then judges its body.
    if !session
        .supports_tool_argument(ATTENTION_TOOL, "candidate_read")
        .unwrap_or(false)
        && let Some(object) = hop.as_object_mut()
    {
        object.remove("candidate_read");
    }
    match session.call_tool(ATTENTION_TOOL, hop, context) {
        Ok(body) => HopOutcome::Answered { snapshot, body },
        Err(error @ OrbitError::ToolNotOnThisHost(_)) => {
            failed(TOOL_MISSING, &error, Some(snapshot))
        }
        Err(
            error @ (OrbitError::InvalidInput(_)
            | OrbitError::RemoteTool {
                code: RemoteToolCode::InvalidInput,
                ..
            }),
        ) => failed(READ_REFUSED, &error, Some(snapshot)),
        Err(error) => failed(UNREACHABLE, &error, Some(snapshot)),
    }
}

fn outcome_snapshot(outcome: &HopOutcome) -> Option<&DestinationSnapshot> {
    match outcome {
        HopOutcome::Answered { snapshot, .. } | HopOutcome::OutOfScope { snapshot } => {
            Some(snapshot)
        }
        HopOutcome::Failed { snapshot, .. } => snapshot.as_ref(),
    }
}

fn qualified_selector(selector: &str) -> bool {
    MachineQualifiedSelector::from_str(selector.trim()).is_ok()
}

/// The workspace echo for one checkout this read opened.
fn echoed_workspace(
    destination: &Destination,
    workspace: &orbit_types::workspace::Workspace,
) -> Value {
    let mut value = json!({
        "id": workspace.id,
        "name": workspace.name,
    });
    if let Some(selector) =
        WorkspaceDescriptor::reachable(destination, workspace.clone()).selector()
    {
        value["selector"] = json!(selector);
    }
    value
}

fn workspace_echo_key(workspace: &Value) -> String {
    workspace
        .get("selector")
        .and_then(Value::as_str)
        .filter(|selector| !selector.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            workspace
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

fn remember_workspace(workspaces: &mut Vec<Value>, workspace: Value) {
    let key = workspace_echo_key(&workspace);
    if key.is_empty()
        || workspaces
            .iter()
            .any(|existing| workspace_echo_key(existing) == key)
    {
        return;
    }
    workspaces.push(workspace);
}

/// Whether the caller asked for `workspace` (every workspace when none are named).
fn caller_names_workspace(
    selectors: &[String],
    workspace: &orbit_types::workspace::Workspace,
) -> bool {
    selectors.is_empty()
        || selectors.iter().any(|selector| {
            let selector = selector.trim();
            match MachineQualifiedSelector::from_str(selector) {
                Ok(qualified) => qualified.workspace_id() == workspace.id,
                Err(_) => selector == workspace.id || selector == workspace.name,
            }
        })
}

/// An owner of a caller-named workspace who was not a destination of this read.
///
/// The destination set is the host filter. Leaving that owner off `degraded`
/// made a replica's empty candidate set look like nothing needed attention
/// [ORB-23333].
fn note_unread_owners(
    snapshot: &DestinationSnapshot,
    selectors: &[String],
    attempted: &BTreeSet<String>,
    host_names: &BTreeMap<String, String>,
    noted: &mut BTreeSet<String>,
    degraded: &mut Vec<AttentionDegraded>,
) {
    for workspace in &snapshot.workspaces {
        if !caller_names_workspace(selectors, workspace) {
            continue;
        }
        let Some(owner) = workspace.owner_machine_id.as_deref() else {
            continue;
        };
        if attempted.contains(owner) || !noted.insert(owner.to_string()) {
            continue;
        }
        let name = host_names
            .get(owner)
            .cloned()
            .unwrap_or_else(|| owner.to_string());
        let label = if workspace.name.is_empty() {
            workspace.id.as_str()
        } else {
            workspace.name.as_str()
        };
        degraded.push(AttentionDegraded {
            scope: format!("host:{name}"),
            kind: AttentionKind::HostUnhealthy.as_str().into(),
            code: "source_unavailable".into(),
            message: format!(
                "{name} owns workspace '{label}' and this read did not include that host"
            ),
        });
    }
}

/// Whether `workspace` on `machine_id` is named by the caller's selectors
/// (every workspace when none are given).
fn in_scope(
    selectors: &[String],
    machine_id: &str,
    workspace: &orbit_types::workspace::Workspace,
) -> bool {
    selectors.is_empty()
        || selectors.iter().any(|selector| {
            let selector = selector.trim();
            match MachineQualifiedSelector::from_str(selector) {
                Ok(qualified) => {
                    // A selector copied from a replica still names the owner's
                    // checkout of that workspace. Matching only the encoded
                    // machine left the owner unread and the page empty
                    // [ORB-23333].
                    qualified.workspace_id() == workspace.id
                        && (qualified.machine_id() == machine_id
                            || workspace.owner_machine_id.as_deref() == Some(machine_id))
                }
                Err(_) => selector == workspace.id || selector == workspace.name,
            }
        })
}

/// The plain selectors this destination's live list matched.
fn matched(selectors: &[String], snapshot: &DestinationSnapshot) -> Vec<String> {
    selectors
        .iter()
        .filter(|selector| {
            snapshot.workspaces.iter().any(|workspace| {
                in_scope(
                    std::slice::from_ref(*selector),
                    &snapshot.machine_id,
                    workspace,
                )
            })
        })
        .cloned()
        .collect()
}

/// The completeness rule: a body is the destination's whole candidate set
/// only when its counts agree with its items and its `item_cap` entries.
fn complete_candidate_set(envelope: &AttentionEnvelope) -> bool {
    let counts = &envelope.counts;
    let returned = envelope.items.len();
    if envelope.candidate_read == Some(true) {
        let cut = envelope
            .degraded
            .iter()
            .any(|entry| entry.code == "item_cap");
        counts.truncated == cut
            && if counts.truncated {
                counts.matching > returned
            } else {
                counts.matching == returned
            }
    } else {
        !counts.truncated && counts.matching == returned
    }
}

fn truncated_entry(host: &AttentionHost, message: String) -> AttentionDegraded {
    AttentionDegraded {
        scope: format!("host:{}", host.name),
        kind: "all".into(),
        code: "response_truncated".into(),
        message,
    }
}

fn host_status(host: &AttentionHost, reachability: &str) -> AttentionHostStatus {
    AttentionHostStatus {
        machine_id: host.machine_id.clone(),
        name: host.name.clone(),
        reachability: reachability.into(),
    }
}

fn severity(owns: bool) -> AttentionSeverity {
    if owns {
        AttentionSeverity::Critical
    } else {
        AttentionSeverity::High
    }
}

/// The item the aggregator builds for a destination it could not read: the
/// destination cannot report on itself.
fn unhealthy_item(host: &AttentionHost, cause: &str, message: &str, owns: bool) -> AttentionItem {
    let first_line = message.lines().next().unwrap_or_default();
    let mut also = vec![next_step(
        "Re-probe every registered host",
        "orbit host list".into(),
    )];
    if cause == TOOL_MISSING {
        also.push(next_step(
            "Update Orbit on this host to a build that serves orbit.attention, then restart its \
             long-lived processes",
            format!("orbit host show '{}'", host.name),
        ));
    }
    let failure = if cause == READ_REFUSED {
        "attention read refused"
    } else {
        "unreachable"
    };
    host_item(
        host,
        cause,
        format!("{} {failure}: {first_line}", host.name),
        json!({"cause": cause, "message": attention_line(message)}),
        severity(owns),
        also,
    )
}

/// A reachable host whose binary or pull protocol differs from this one's,
/// as `orbit host list` reports skew.
fn skew_item(
    host: &AttentionHost,
    snapshot: &DestinationSnapshot,
    local: Option<&crate::HostFacts>,
    owns: bool,
) -> Option<AttentionItem> {
    let local = local?;
    if snapshot.machine_id == local.machine_id {
        return None;
    }
    let differs = |theirs: &Option<String>, ours: &Option<String>| {
        theirs.is_some() && ours.is_some() && theirs != ours
    };
    let mut fields = Vec::new();
    if differs(&snapshot.host.binary_version, &local.binary_version) {
        fields.push(format!(
            "binary_version {} (this host {})",
            snapshot.host.binary_version.as_deref().unwrap_or_default(),
            local.binary_version.as_deref().unwrap_or_default()
        ));
    }
    if differs(
        &snapshot.host.protocol_fingerprint,
        &local.protocol_fingerprint,
    ) {
        fields.push("protocol_fingerprint".to_string());
    }
    if fields.is_empty() {
        return None;
    }
    Some(host_item(
        host,
        "version_skew",
        format!("{} differs in {}", host.name, fields.join(" and ")),
        json!({
            "cause": "version_skew",
            "binary_version": snapshot.host.binary_version,
            "protocol_fingerprint": snapshot.host.protocol_fingerprint,
        }),
        severity(owns),
        vec![next_step(
            "Deploy matching Orbit builds on both hosts and restart their long-lived processes",
            "orbit host list".into(),
        )],
    ))
}

fn host_item(
    host: &AttentionHost,
    cause: &str,
    reason: String,
    detail: Value,
    severity: AttentionSeverity,
    also: Vec<AttentionNext>,
) -> AttentionItem {
    let kind = AttentionKind::HostUnhealthy;
    let identity = json!([kind, host.machine_id, "", cause]);
    let mut item = AttentionItem {
        id: format!(
            "att_{}_{}",
            kind.as_str(),
            cursor::attention_digest(identity.to_string().as_bytes())
        ),
        kind,
        severity,
        workspace: AttentionWorkspace::none(),
        host: host.clone(),
        refs: json!({}),
        task: None,
        since: None,
        age_seconds: None,
        expires_at: None,
        reason: attention_line(&reason),
        detail,
        next: AttentionNext {
            also,
            ..next_step(
                "Diagnose the host",
                format!("orbit host show '{}'", host.name),
            )
        },
        fingerprint: String::new(),
    };
    item.fingerprint = cursor::attention_fingerprint(&item, json!({"cause": cause}));
    item
}

fn next_step(summary: &str, command: String) -> AttentionNext {
    AttentionNext {
        summary: summary.into(),
        tool: None,
        input: None,
        command,
        needs: Vec::new(),
        also: Vec::new(),
    }
}

/// One line of at most 240 characters, ending in `…` when cut.
fn attention_line(source: &str) -> String {
    let line = source.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= 240 {
        return line.to_string();
    }
    let mut cut = line.chars().take(239).collect::<String>();
    cut.push('…');
    cut
}
