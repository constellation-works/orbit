//! The public response: ordering, delta cursor and limit, applied once by the
//! outermost responder (one host's local call, or the federated aggregator
//! after it merged every destination's candidate set).
use super::cursor;
use chrono::{DateTime, Utc};
use orbit_types::tool::{
    AttentionCounts, AttentionCursor, AttentionCursorStatus, AttentionDegraded, AttentionEnvelope,
    AttentionHostStatus, AttentionItem,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The cap on hashes one cursor carries; older entries beyond it are dropped.
const CURSOR_SEEN_CAP: usize = 1000;

/// Severity descending, then `since` ascending (`null` last), then `id`.
pub fn attention_order(items: &mut [AttentionItem]) {
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

/// Everything a public response is built from: the complete merged candidate
/// set, the sources that could not be read, and the echoed filters.
pub struct AttentionPageInput {
    /// Echoed filters with resolved workspaces; never the cursor.
    pub filters: Value,
    /// Every post-filter candidate, in any order.
    pub items: Vec<AttentionItem>,
    /// Sources and destinations that could not be read completely.
    pub degraded: Vec<AttentionDegraded>,
    /// Per-host reachability, present when more than the local host was in scope.
    pub hosts: Vec<AttentionHostStatus>,
}

/// Cursor parameters of the outermost responder.
pub struct AttentionPageCursor<'a> {
    /// The caller's `since` token, if any.
    pub since: Option<&'a str>,
    /// Public response limit.
    pub limit: usize,
}

/// Order the candidates, select the delta against `since`, cut the public
/// limit and mint the next cursor.
///
/// When any source was degraded the next cursor also carries every hash the
/// caller's cursor held, so items from a source that comes back are not
/// re-injected (attention-cursor rule 4).
pub fn attention_public_page(
    input: AttentionPageInput,
    cursor_input: AttentionPageCursor<'_>,
    now: DateTime<Utc>,
) -> AttentionEnvelope {
    let AttentionPageInput {
        filters,
        mut items,
        degraded,
        hosts,
    } = input;
    attention_order(&mut items);
    let issued = now.timestamp().max(0) as u64;
    let (status, seen) = cursor::decode_attention_cursor(cursor_input.since, issued);
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
        .take(cursor_input.limit)
        .cloned()
        .collect::<Vec<_>>();
    let returned_hashes = returned
        .iter()
        .map(cursor::attention_seen_hash)
        .collect::<BTreeSet<_>>();
    let mut next_seen = Vec::new();
    let mut kept = BTreeSet::new();
    for item in &items {
        let hash = cursor::attention_seen_hash(item);
        if (seen.contains(&hash) || returned_hashes.contains(&hash)) && kept.insert(hash) {
            next_seen.push(hash);
        }
    }
    if !degraded.is_empty() {
        for hash in &seen {
            if kept.insert(*hash) {
                next_seen.push(*hash);
            }
        }
    }
    let cursor_truncated = next_seen.len() > CURSOR_SEEN_CAP;
    next_seen.truncate(CURSOR_SEEN_CAP);
    let read_scope = empty_read_scope(&filters, &hosts);
    let message = if returned.is_empty() && !degraded.is_empty() {
        format!(
            "No items found; {} sources could not be read; {read_scope}",
            degraded.len()
        )
    } else if returned.is_empty() && status == AttentionCursorStatus::Accepted {
        format!("No new or changed items ({unchanged} unchanged); {read_scope}")
    } else if returned.is_empty() {
        format!("Nothing needs attention; {read_scope}")
    } else {
        format!("{} items need attention", returned.len())
    };
    let mut by_kind = BTreeMap::new();
    let mut by_severity = BTreeMap::new();
    for item in &returned {
        *by_kind.entry(item.kind).or_default() += 1;
        *by_severity.entry(item.severity).or_default() += 1;
    }
    AttentionEnvelope {
        schema_version: 1,
        generated_at: now,
        candidate_read: None,
        filters,
        counts: AttentionCounts {
            matching,
            returned: returned.len(),
            truncated: matching > returned.len(),
            by_kind,
            by_severity,
            unchanged: (status == AttentionCursorStatus::Accepted).then_some(unchanged),
        },
        cursor: AttentionCursor {
            next: cursor::encode_attention_cursor(issued, &next_seen),
            status,
            truncated: cursor_truncated.then_some(true),
        },
        items: returned,
        hosts,
        degraded,
        message,
    }
}

/// Hosts and workspaces this page actually read, for an empty result.
///
/// Unreachable hosts are omitted: they were not read. A workspace label is its
/// name, else its id.
fn empty_read_scope(filters: &Value, hosts: &[AttentionHostStatus]) -> String {
    let mut host_names = Vec::new();
    for host in hosts {
        if host.reachability == "reachable"
            && !host.name.is_empty()
            && !host_names.iter().any(|name| name == &host.name)
        {
            host_names.push(host.name.clone());
        }
    }
    let mut workspace_names = Vec::new();
    if let Some(listed) = filters.get("workspace").and_then(Value::as_array) {
        for workspace in listed {
            let label = workspace
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .or_else(|| workspace.get("id").and_then(Value::as_str))
                .or_else(|| workspace.as_str())
                .unwrap_or("")
                .trim();
            if !label.is_empty() && !workspace_names.iter().any(|name| name == label) {
                workspace_names.push(label.to_string());
            }
        }
    }
    match (host_names.is_empty(), workspace_names.is_empty()) {
        (true, true) => "no host or workspace was read".to_string(),
        (false, true) => format!("hosts read: {}", host_names.join(", ")),
        (true, false) => format!("workspaces read: {}", workspace_names.join(", ")),
        (false, false) => format!(
            "hosts read: {}; workspaces read: {}",
            host_names.join(", "),
            workspace_names.join(", ")
        ),
    }
}
