//! Read-only operator attention: local scopes, source derivation and the
//! destination side of federation. Filters, the cursor and the public page
//! live with the operation in `orbit_common::governance::attention`, so the
//! federated aggregator applies the same rules once over every host.
mod host;
mod notice_sources;
mod provider_sources;
mod runtime_sources;
mod sources;

use crate::{
    OrbitRuntime,
    runtime::authorization::resolved_caller_capabilities,
    runtime::workspace::catalog::{FederatedWorkspaceTarget, WorkspaceScope},
};
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::governance::attention::{
    cursor,
    filter::{AttentionFilter, attention_strings},
    page::{AttentionPageCursor, AttentionPageInput, attention_order, attention_public_page},
};
use orbit_types::tool::{
    AttentionCounts, AttentionCursor, AttentionCursorStatus, AttentionDegraded, AttentionEnvelope,
    AttentionHost, AttentionKind, AttentionWorkspace, McpCapability, ToolSessionContext,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Strictly increasing severity age thresholds, in hours.
///
/// The former `attention.age_bump_hours` default, fixed when the key was removed [ORB-22808].
const ATTENTION_AGE_BUMP_HOURS: [u32; 2] = [24, 72];
/// Failed-run lookback, in hours.
///
/// The former `attention.run_window_hours` default, fixed when the key was removed [ORB-22808].
const ATTENTION_RUN_WINDOW_HOURS: u32 = 48;
/// Wall-clock minutes after which a running run counts as stuck.
///
/// The former `attention.run_stuck_after_minutes` default, fixed when the key was removed [ORB-22808].
const ATTENTION_RUN_STUCK_AFTER_MINUTES: u32 = 120;
/// Provider CI read cache lifetime, in seconds.
///
/// The former `attention.ci_cache_seconds` default, fixed when the key was removed [ORB-22808].
const ATTENTION_CI_CACHE_SECONDS: u64 = 60;
/// Per-source read deadline, in seconds.
///
/// The former `attention.source_timeout_seconds` default, fixed when the key was removed [ORB-22808].
const ATTENTION_SOURCE_TIMEOUT_SECONDS: u64 = 10;
/// Maximum candidates one workspace contributes.
///
/// The former `attention.per_workspace_item_cap` default, fixed when the key was removed [ORB-22808].
const ATTENTION_PER_WORKSPACE_ITEM_CAP: usize = 200;

fn attention_local_workspace(runtime: &OrbitRuntime) -> Result<AttentionWorkspace, OrbitError> {
    let id = match runtime.workspace_runtime_binding() {
        Some(binding) => binding.logical_workspace_id.clone(),
        None => runtime.workspace_id()?,
    };
    let name = if let Some(catalog) = runtime.workspace_catalog() {
        catalog
            .resolve_scope(&WorkspaceScope::Selectors(vec![id.clone()]))?
            .first()
            .map(|target| target.name.clone())
            .unwrap_or_else(|| runtime.workspace_label())
    } else {
        runtime.workspace_label()
    };
    Ok(AttentionWorkspace::local(id, name))
}
/// Derive attention from the authoritative local workspace catalog: the
/// public response, or with `candidate_read` the aggregator's whole
/// post-filter candidate set. No records, configuration or cursor state are
/// written by this operation.
///
/// Each checkout reports what its host is authoritative for: an owner its
/// task-level kinds, every checkout its own host health.
pub fn attention_query(
    runtime: &OrbitRuntime,
    input: Value,
    session_context: &ToolSessionContext,
) -> Result<Value, OrbitError> {
    let filter = AttentionFilter::parse(input)?;
    let reads_host_health = filter.selected(AttentionKind::HostUnhealthy)
        && resolved_caller_capabilities(session_context).contains(&McpCapability::Operator);
    let operator = resolved_caller_capabilities(session_context).contains(&McpCapability::Operator);
    let machine = orbit_config::load_machine_settings(&runtime.global_root())?;
    let host = AttentionHost {
        machine_id: runtime
            .automation_machine_identity()
            .map(str::to_owned)
            .or(machine.id)
            .unwrap_or_else(|| "local".into()),
        name: runtime
            .automation_execution_location()
            .and_then(|location| location.machine_name.clone())
            .or(machine.name)
            .unwrap_or_else(|| "local".into()),
    };
    let hosts = attention_strings(&filter.input, "host");
    let known_hosts = runtime
        .workspace_catalog()
        .map(|catalog| catalog.attention_registered_hosts())
        .transpose()?
        .unwrap_or_default();
    for selector in &hosts {
        if selector != &host.machine_id
            && selector != &host.name
            && !known_hosts
                .iter()
                .any(|host| selector == &host.name || selector == &host.machine_id)
        {
            return Err(OrbitError::InvalidInput(format!(
                "unknown_host: {selector}; this local attention scope serves {} ({})",
                host.name, host.machine_id
            )));
        }
    }
    let selectors = attention_strings(&filter.input, "workspace");
    let all = filter.input["all"] == true
        || (selectors.is_empty()
            && runtime.workspace_runtime_binding().is_none()
            && runtime.workspace_catalog().is_some());
    let scope = WorkspaceScope::from_inputs(selectors.clone(), all);
    // An unbound host runtime lends the registry, but has no task partition.
    let current = if runtime.workspace_runtime_binding().is_some()
        || !scope.is_federated()
        || runtime.workspace_catalog().is_none()
    {
        Some(attention_local_workspace(runtime)?)
    } else {
        None
    };
    let current_target = || {
        current
            .as_ref()
            .map(|current| FederatedWorkspaceTarget {
                workspace_id: current.id.clone(),
                name: current.name.clone(),
                repo_root: runtime.paths().repo_root.clone(),
            })
            .ok_or_else(|| {
                OrbitError::InvalidInput(
                    "attention has no current workspace; select a registered workspace or all"
                        .into(),
                )
            })
    };
    let mut opened = Vec::new();
    let mut workspaces = Vec::new();
    let mut items = Vec::new();
    let mut degraded = Vec::new();
    let now = Utc::now();
    let mut source_costs = BTreeMap::new();
    let mut matching_before_cap = 0usize;
    let mut capped = false;
    let targets = if matches!(scope, WorkspaceScope::Current) {
        vec![current_target()?]
    } else if let Some(catalog) = runtime.workspace_catalog() {
        catalog
            .resolve_scope(&scope)
            .map_err(|error| OrbitError::InvalidInput(format!("unknown_workspace: {error}")))?
    } else if !all
        && current.as_ref().is_some_and(|current| {
            selectors
                .iter()
                .all(|selector| selector == &current.id || selector == &current.name)
        })
    {
        vec![current_target()?]
    } else {
        return Err(OrbitError::InvalidInput(
            "unknown_workspace: a registered workspace catalog is required for this scope".into(),
        ));
    };
    // A host filter that names only other machines used to clear the targets
    // and return an empty page. The caller then read that as all-clear
    // [ORB-23333]. This process still cannot answer for another host; it says
    // which hosts it did not read.
    let names_local_host = hosts
        .iter()
        .any(|selector| selector == &host.name || selector == &host.machine_id);
    let foreign_hosts = hosts
        .iter()
        .filter(|selector| *selector != &host.name && *selector != &host.machine_id)
        .cloned()
        .collect::<Vec<_>>();
    let targets = if !hosts.is_empty() && !names_local_host {
        Vec::new()
    } else {
        targets
    };
    let mut seen_workspaces = BTreeSet::new();
    for target in targets {
        if !seen_workspaces.insert(target.workspace_id.clone()) {
            continue;
        }
        let workspace = AttentionWorkspace::local(target.workspace_id.clone(), target.name.clone());
        workspaces.push(workspace.clone());
        let selected = if current
            .as_ref()
            .is_some_and(|current| target.workspace_id == current.id)
        {
            runtime
        } else {
            let Some(catalog) = runtime.workspace_catalog() else {
                continue;
            };
            match catalog.open_observational(&target) {
                Ok(kernel) => {
                    opened.push(OrbitRuntime::from_kernel(kernel));
                    &opened[opened.len() - 1]
                }
                Err(error) => {
                    for kind in &filter.kinds {
                        degraded.push(AttentionDegraded {
                            scope: format!("workspace:{}", workspace.id),
                            kind: kind.as_str().into(),
                            code: "source_unavailable".into(),
                            message: error.to_string(),
                        });
                    }
                    continue;
                }
            }
        };
        let mut candidates =
            match sources::read_attention_sources(selected, &workspace, &host, &filter, now) {
                Ok(candidates) => candidates,
                Err(error) => {
                    for kind in filter.kinds.iter().filter(|kind| {
                        AttentionKind::TASK_LEVEL_NAMES.contains(&kind.as_str())
                            || **kind == AttentionKind::NoticeOpen
                    }) {
                        degraded.push(AttentionDegraded {
                            scope: format!("workspace:{}", workspace.id),
                            kind: kind.as_str().into(),
                            code: "source_unavailable".into(),
                            message: error.to_string(),
                        });
                    }
                    sources::AttentionCandidates {
                        items: Vec::new(),
                        degraded: Vec::new(),
                        timings: BTreeMap::new(),
                    }
                }
            };
        runtime_sources::read_runtime_attention(
            selected,
            &workspace,
            &host,
            &filter,
            now,
            operator,
            &mut candidates,
        );
        provider_sources::read_ci_attention(
            selected,
            &workspace,
            &host,
            &filter,
            now,
            session_context,
            &mut candidates,
        );
        if reads_host_health {
            let owner = selected.coordination_task_reads_visible();
            match host::read_doctor_host_item(runtime, selected, &workspace, &host, owner) {
                Ok(Some(item)) => candidates.items.push(item),
                Ok(None) => {}
                Err(error) => candidates.degraded.push(AttentionDegraded {
                    scope: format!("workspace:{}", workspace.id),
                    kind: AttentionKind::HostUnhealthy.as_str().into(),
                    code: "source_unavailable".into(),
                    message: error.to_string(),
                }),
            }
        }
        source_costs.insert(workspace.id.clone(), candidates.timings);
        candidates.items.retain(|item| filter.matches(item));
        matching_before_cap += candidates.items.len();
        attention_order(&mut candidates.items);
        let cap = ATTENTION_PER_WORKSPACE_ITEM_CAP;
        if candidates.items.len() > cap {
            let dropped = candidates.items[cap..]
                .iter()
                .map(|item| item.kind.as_str())
                .collect::<BTreeSet<_>>();
            for kind in dropped {
                candidates.degraded.push(AttentionDegraded {
                    scope: format!("workspace:{}", workspace.id),
                    kind: kind.into(),
                    code: "item_cap".into(),
                    message: format!("Per-workspace candidate cap {cap} reached"),
                });
            }
            candidates.items.truncate(cap);
            capped = true;
        }
        items.extend(candidates.items);
        degraded.extend(candidates.degraded);
    }
    for selector in &foreign_hosts {
        degraded.push(AttentionDegraded {
            scope: format!("host:{selector}"),
            kind: AttentionKind::HostUnhealthy.as_str().into(),
            code: "source_unavailable".into(),
            message: format!(
                "this read is local to {} ({}) and did not read host {selector}",
                host.name, host.machine_id
            ),
        });
    }
    if filter.selected(AttentionKind::HostUnhealthy) && !reads_host_health {
        degraded.push(host::host_capability_degraded(&host));
    }
    tracing::debug!(target: "orbit.attention", source_microseconds = ?source_costs, "attention source costs");
    let mut echoed = filter.input.clone();
    if let Some(echoed) = echoed.as_object_mut() {
        echoed.remove("since");
        echoed.remove("model");
        echoed.remove("agent");
        echoed.remove("candidate_read");
    }
    echoed["workspace"] = serde_json::json!(workspaces);
    let response = if filter.candidate_read {
        attention_candidate_envelope(echoed, items, degraded, matching_before_cap, capped, now)
    } else {
        attention_public_page(
            AttentionPageInput {
                filters: echoed,
                items,
                degraded,
                hosts: Vec::new(),
            },
            AttentionPageCursor {
                since: filter.input["since"].as_str(),
                limit: filter.limit,
            },
            now,
        )
    };
    serde_json::to_value(response)
        .map_err(|error| OrbitError::Execution(format!("serialize attention response: {error}")))
}

/// The candidate read: every post-filter item, bounded only by the
/// per-workspace cap. `matching` counts before that cap and `truncated` is
/// true exactly when it cut, so the aggregator can judge completeness. No
/// cursor or public limit applies; the cursor is minted empty and discarded.
fn attention_candidate_envelope(
    filters: Value,
    mut items: Vec<orbit_types::tool::AttentionItem>,
    degraded: Vec<AttentionDegraded>,
    matching: usize,
    truncated: bool,
    now: chrono::DateTime<Utc>,
) -> AttentionEnvelope {
    attention_order(&mut items);
    let mut by_kind = BTreeMap::new();
    let mut by_severity = BTreeMap::new();
    for item in &items {
        *by_kind.entry(item.kind).or_default() += 1;
        *by_severity.entry(item.severity).or_default() += 1;
    }
    AttentionEnvelope {
        schema_version: 1,
        generated_at: now,
        candidate_read: Some(true),
        filters,
        counts: AttentionCounts {
            matching,
            returned: items.len(),
            truncated,
            by_kind,
            by_severity,
            unchanged: None,
        },
        cursor: AttentionCursor {
            next: cursor::encode_attention_cursor(now.timestamp().max(0) as u64, &[]),
            status: AttentionCursorStatus::None,
            truncated: None,
        },
        message: format!("{} candidate items", items.len()),
        items,
        hosts: Vec::new(),
        degraded,
    }
}
