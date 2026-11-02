//! Read-only operator attention: local scopes, source derivation, filters and deltas.
mod cursor;
mod filter;
mod sources;

use crate::{
    OrbitRuntime,
    runtime::workspace::catalog::{FederatedWorkspaceTarget, WorkspaceScope},
};
use chrono::Utc;
use filter::{AttentionFilter, attention_strings};
use orbit_common::OrbitError;
use orbit_types::tool::{
    AttentionCounts, AttentionCursor, AttentionCursorStatus, AttentionDegraded, AttentionEnvelope,
    AttentionHost, AttentionItem, AttentionWorkspace,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn attention_order(items: &mut [AttentionItem]) {
    items.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| match (&a.since, &b.since) {
                (Some(a), Some(b)) => a.cmp(b),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then_with(|| a.id.cmp(&b.id))
    });
}
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
    Ok(AttentionWorkspace { id, name })
}
/// Derive a public attention response from the authoritative local workspace catalog.
/// No records, configuration or cursor state are written by this operation.
pub fn attention_query(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    let filter = AttentionFilter::parse(input)?;
    let settings = runtime.context.settings().attention_settings();
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
    let targets = if !hosts.is_empty()
        && !hosts
            .iter()
            .any(|selector| selector == &host.name || selector == &host.machine_id)
    {
        Vec::new()
    } else {
        targets
    };
    let mut seen_workspaces = BTreeSet::new();
    for target in targets {
        if !seen_workspaces.insert(target.workspace_id.clone()) {
            continue;
        }
        let workspace = AttentionWorkspace {
            id: target.workspace_id.clone(),
            name: target.name.clone(),
        };
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
        let mut candidates = match sources::read_attention_sources(
            selected,
            &workspace,
            &host,
            &filter,
            now,
            selected.context.settings().attention_settings(),
        ) {
            Ok(candidates) => candidates,
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
        };
        source_costs.insert(workspace.id.clone(), candidates.timings);
        candidates.items.retain(|item| filter.matches(item));
        attention_order(&mut candidates.items);
        let cap = selected
            .context
            .settings()
            .attention_settings()
            .per_workspace_item_cap as usize;
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
        }
        items.extend(candidates.items);
        degraded.extend(candidates.degraded);
    }
    tracing::debug!(target: "orbit.attention", source_microseconds = ?source_costs, "attention source costs");
    attention_order(&mut items);
    let (status, seen) = cursor::decode_attention_cursor(
        filter.input["since"].as_str(),
        now.timestamp().max(0) as u64,
        settings.cursor_ttl_hours,
    );
    let unchanged = items
        .iter()
        .filter(|item| seen.contains(&cursor::attention_seen_hash(item)))
        .count();
    let delta = items
        .iter()
        .filter(|item| !seen.contains(&cursor::attention_seen_hash(item)))
        .collect::<Vec<_>>();
    let matching = delta.len();
    let returned = delta
        .into_iter()
        .take(filter.limit)
        .cloned()
        .collect::<Vec<_>>();
    let returned_hashes = returned
        .iter()
        .map(cursor::attention_seen_hash)
        .collect::<BTreeSet<_>>();
    let mut next_seen = Vec::new();
    for item in &items {
        let hash = cursor::attention_seen_hash(item);
        if (seen.contains(&hash) || returned_hashes.contains(&hash)) && !next_seen.contains(&hash) {
            next_seen.push(hash);
        }
    }
    if !degraded.is_empty() {
        for hash in &seen {
            if !next_seen.contains(hash) {
                next_seen.push(*hash);
            }
        }
    }
    let cursor_truncated = next_seen.len() > 1000;
    next_seen.truncate(1000);
    let mut echoed = filter.input.clone();
    if let Some(echoed) = echoed.as_object_mut() {
        echoed.remove("since");
        echoed.remove("model");
        echoed.remove("agent");
    }
    echoed["workspace"] = json!(workspaces);
    let message = if returned.is_empty() && !degraded.is_empty() {
        format!(
            "No items found; {} sources could not be read",
            degraded.len()
        )
    } else if returned.is_empty() && status == AttentionCursorStatus::Accepted {
        format!("No new or changed items ({unchanged} unchanged)")
    } else if returned.is_empty() {
        format!("Nothing needs attention; filters: {echoed}")
    } else {
        format!("{} items need attention", returned.len())
    };
    let mut by_kind = BTreeMap::new();
    let mut by_severity = BTreeMap::new();
    for item in &returned {
        *by_kind.entry(item.kind).or_default() += 1;
        *by_severity.entry(item.severity).or_default() += 1;
    }
    let response = AttentionEnvelope {
        schema_version: 1,
        generated_at: now,
        filters: echoed,
        counts: AttentionCounts {
            matching,
            returned: returned.len(),
            truncated: matching > returned.len(),
            by_kind,
            by_severity,
            unchanged: (status == AttentionCursorStatus::Accepted).then_some(unchanged),
        },
        cursor: AttentionCursor {
            next: cursor::encode_attention_cursor(now.timestamp().max(0) as u64, &next_seen),
            status,
            truncated: cursor_truncated.then_some(true),
        },
        items: returned,
        degraded,
        message,
    };
    serde_json::to_value(response)
        .map_err(|error| OrbitError::Execution(format!("serialize attention response: {error}")))
}
