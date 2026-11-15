//! Workspace-less `orbit.notice.list` on the federated mux: one
//! owner-local aggregate read per owner machine, merged by the caller.
//!
//! Each hop is an ordinary routed workspace call carrying `all: true`
//! (every workspace homed on that owner) and the item filters, but no
//! limit; this caller deduplicates by notice id, orders, and only then
//! applies `limit`. An owner that does not answer becomes a `degraded`
//! entry naming the host — and the workspaces it listed, when it listed
//! any — never a silently shorter list. This is the minimal per-owner
//! fan-out the notice federation spec calls for until the attention
//! federation's shared aggregation lands; fold the two then.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::governance::notice::{NOTICE_LIST_DEFAULT_LIMIT, notice_list_limit_in_call};
use orbit_types::record::{NoticeKind, NoticeListOrderKey};
use orbit_types::tool::{ToolSessionContext, mcp_advertised_tool_name};
use serde_json::{Value, json};

use super::descriptor::WorkspaceDescriptor;
use super::host::{FederatedMcpHost, workspace_selector};

/// Whether `name` is the notice list tool, either spelling.
fn is_notice_list(name: &str) -> bool {
    mcp_advertised_tool_name(name) == "orbit_notice_list"
}

/// One owner's place in the fan-out, in configured destination order.
struct OwnerRead {
    label: String,
    selector: Option<String>,
    workspaces: Vec<String>,
}

impl FederatedMcpHost {
    /// Answer a workspace-less notice list by aggregating every owner, or
    /// `None` when this call is not one: another tool, a call that names a
    /// workspace (input or session), or a worker-bound call, whose owner
    /// route stays single-owner.
    pub(super) fn aggregate_notice_list(
        &self,
        name: &str,
        input: Value,
        session_context: ToolSessionContext,
    ) -> Option<Result<Value, OrbitError>> {
        if !is_notice_list(name) || session_context.worker_invocation.is_some() {
            return None;
        }
        match workspace_selector(&input, &session_context) {
            Ok(Some(_)) => None,
            Ok(None) => Some(self.fan_out_notice_list(name, input, session_context)),
            // A malformed selector is the routed call's refusal to make.
            Err(_) => None,
        }
    }

    fn fan_out_notice_list(
        &self,
        name: &str,
        input: Value,
        session_context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let limit = notice_list_limit_in_call(&input)?.unwrap_or(NOTICE_LIST_DEFAULT_LIMIT);
        let descriptors = self.probe_all_destinations(false);
        let owners = self.owner_reads(&descriptors);

        let mut degraded: Vec<String> = owners
            .iter()
            .filter(|owner| owner.selector.is_none())
            .map(|owner| owner.degraded_label())
            .collect();

        // One aggregate read per owner, issued in parallel so the list
        // costs the slowest owner rather than the sum of all of them.
        let reads: Vec<(&OwnerRead, Value)> = owners
            .iter()
            .filter_map(|owner| owner.selector.as_ref().map(|selector| (owner, selector)))
            .map(|(owner, selector)| {
                let mut call = input.clone();
                if let Some(object) = call.as_object_mut() {
                    object.insert("workspace".into(), Value::String(selector.clone()));
                    object.insert("all".into(), Value::Bool(true));
                    object.remove("limit");
                }
                (owner, call)
            })
            .collect();
        let answers = std::thread::scope(|scope| {
            let calls = reads
                .iter()
                .map(|(owner, call)| {
                    let call = call.clone();
                    let session = session_context.clone();
                    (
                        *owner,
                        scope.spawn(move || self.route_workspace_call(name, call, session, false)),
                    )
                })
                .collect::<Vec<_>>();
            calls
                .into_iter()
                .map(|(owner, call)| {
                    (
                        owner,
                        call.join().unwrap_or_else(|_| {
                            Err(OrbitError::UnreachableDestination(format!(
                                "{}: notice read panicked",
                                owner.label
                            )))
                        }),
                    )
                })
                .collect::<Vec<_>>()
        });

        let mut notices: Vec<Value> = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for (owner, answer) in answers {
            let answer = match answer {
                Ok(answer) => answer,
                Err(error) => {
                    tracing::warn!(
                        host = %owner.label,
                        %error,
                        "owner did not answer the workspace-less notice list",
                    );
                    degraded.push(owner.degraded_label());
                    continue;
                }
            };
            if let Some(owner_degraded) = answer.get("degraded").and_then(Value::as_array) {
                degraded.extend(
                    owner_degraded
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned),
                );
            }
            let Some(list) = answer.get("notices").and_then(Value::as_array) else {
                degraded.push(owner.degraded_label());
                continue;
            };
            for notice in list {
                let Some(id) = notice.get("id").and_then(Value::as_str) else {
                    continue;
                };
                if seen.insert(id.to_string()) {
                    notices.push(notice.clone());
                }
            }
        }

        sort_in_notice_list_order(&mut notices);
        notices.truncate(limit);
        degraded.sort();
        degraded.dedup();
        Ok(json!({ "notices": notices, "degraded": degraded }))
    }

    /// One read per owner machine. Owners are the machines the listed
    /// workspaces name as their owner, not merely the destinations: a
    /// reachable follower that owns nothing listed contributes no read
    /// and no `degraded` entry, while a destination that does not answer
    /// (it may own workspaces nobody listed) and an owner with no
    /// configured destination both degrade rather than vanish.
    fn owner_reads(&self, descriptors: &[WorkspaceDescriptor]) -> Vec<OwnerRead> {
        let owned_workspaces = |owner: &str| -> Vec<String> {
            let mut names: Vec<String> = descriptors
                .iter()
                .filter(|row| row.workspace_owner_machine_id() == Some(owner))
                .filter_map(|row| row.workspace_name().map(ToOwned::to_owned))
                .collect();
            names.sort();
            names.dedup();
            names
        };
        let mut reads: Vec<OwnerRead> = Vec::new();
        let mut covered: BTreeSet<&str> = BTreeSet::new();
        for destination in self.destinations() {
            covered.insert(destination.machine_id.as_str());
            let rows: Vec<&WorkspaceDescriptor> = descriptors
                .iter()
                .filter(|row| row.machine_id() == destination.machine_id)
                .collect();
            let workspaces = owned_workspaces(&destination.machine_id);
            let selector = rows
                .iter()
                .filter(|row| {
                    row.is_reachable() && row.has_active_checkout() && row.holds_control_plane()
                })
                .find_map(|row| row.selector().map(ToOwned::to_owned));
            let answered = rows.iter().any(|row| row.is_reachable());
            if selector.is_none() && answered && workspaces.is_empty() {
                // A reachable follower: its workspaces are read through
                // their owners, and it is no one's degraded entry.
                continue;
            }
            reads.push(OwnerRead {
                label: format!(
                    "{} ({})",
                    destination.machine_name_display(),
                    destination.machine_id
                ),
                selector,
                workspaces,
            });
        }
        let mut unconfigured: Vec<&str> = descriptors
            .iter()
            .filter_map(|row| row.workspace_owner_machine_id())
            .filter(|owner| !covered.contains(owner))
            .collect();
        unconfigured.sort_unstable();
        unconfigured.dedup();
        reads.extend(unconfigured.into_iter().map(|owner| OwnerRead {
            label: owner.to_string(),
            selector: None,
            workspaces: owned_workspaces(owner),
        }));
        reads
    }
}

impl OwnerRead {
    /// The `degraded` entry for an owner that contributed nothing: the
    /// host, plus the workspaces it listed when it listed any.
    fn degraded_label(&self) -> String {
        if self.workspaces.is_empty() {
            self.label.clone()
        } else {
            format!("{}: {}", self.label, self.workspaces.join(", "))
        }
    }
}

/// Order merged notices in the shared notice list order. Every owner
/// answers from a typed notice, so the key reads; a notice whose key does
/// not (a kind this build does not know) follows the rest, by id.
fn sort_in_notice_list_order(notices: &mut [Value]) {
    notices.sort_by(|left, right| {
        match (notice_list_order_key(left), notice_list_order_key(right)) {
            (Some(left), Some(right)) => left.cmp(&right),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => notice_id(left).cmp(&notice_id(right)),
        }
    });
}

fn notice_id(notice: &Value) -> Option<&str> {
    notice.get("id").and_then(Value::as_str)
}

/// Read a wire notice's list-order key.
fn notice_list_order_key(notice: &Value) -> Option<NoticeListOrderKey<'_>> {
    Some(NoticeListOrderKey {
        kind: notice
            .get("kind")
            .and_then(Value::as_str)
            .and_then(NoticeKind::parse),
        updated_at: notice
            .get("updated_at")
            .and_then(Value::as_str)
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|at| at.with_timezone(&Utc)),
        id: notice_id(notice)?,
    })
}
