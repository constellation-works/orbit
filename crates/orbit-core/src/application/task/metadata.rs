//! Internal bodyless owner reads. This is not a public list pagination API.
use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::TaskStoreBackend;
use orbit_store::contracts::TaskListFilter;
use orbit_types::task::{Task, TaskEnvelopeV2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::listing::metadata_task;
use crate::OrbitRuntime;

// Leave ample room for MCP's text/structured result framing under its 64 MiB cap.
const MAX_METADATA_ROWS: usize = 128;
const MAX_METADATA_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataCursor {
    created_at: DateTime<Utc>,
    id: String,
}

impl MetadataCursor {
    fn of(task: &TaskEnvelopeV2) -> Self {
        Self {
            created_at: task.created_at,
            id: task.id.clone(),
        }
    }

    fn precedes(&self, next: &Self) -> bool {
        self.created_at > next.created_at
            || (self.created_at == next.created_at && self.id < next.id)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataRequest {
    schema_version: u32,
    ids: Option<BTreeSet<String>>,
    scan_id: Option<String>,
    cursor: Option<MetadataCursor>,
    max_rows: usize,
    max_bytes: usize,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataPage {
    schema_version: u32,
    scan_id: String,
    total: usize,
    remaining: usize,
    complete: bool,
    next: Option<MetadataCursor>,
    items: Vec<TaskEnvelopeV2>,
}

fn invalid_metadata(message: &str) -> OrbitError {
    OrbitError::Store(format!("owner task metadata unavailable: {message}"))
}

fn encode_metadata<T: Serialize>(value: &T) -> Result<Vec<u8>, OrbitError> {
    serde_json::to_vec(value).map_err(|_| invalid_metadata("could not encode page"))
}

/// Serve only through the authenticated host projection, after owner/workspace
/// checks. A stale/unavailable index is a refusal, never a full-bundle fallback.
pub(crate) fn owner_metadata_page(
    runtime: &OrbitRuntime,
    input: &Value,
) -> Result<Value, OrbitError> {
    let request: MetadataRequest = serde_json::from_value(input["metadata_page"].clone())
        .map_err(|_| invalid_metadata("malformed page request"))?;
    if request.schema_version != 1
        || !(1..=MAX_METADATA_ROWS).contains(&request.max_rows)
        || !(512..=MAX_METADATA_BYTES).contains(&request.max_bytes)
        || request
            .ids
            .as_ref()
            .is_some_and(|ids| ids.len() > MAX_METADATA_ROWS)
        || (request.cursor.is_some() && request.scan_id.is_none())
    {
        return Err(invalid_metadata("unsupported page request or limits"));
    }
    if !runtime.coordination_task_reads_visible() {
        return Err(invalid_metadata("workspace task reads are unavailable"));
    }
    let store = runtime.stores().tasks();
    let workspace = runtime.workspace_id()?;
    let mut result = None;
    store.in_read_section(&mut || {
        result = Some(build_metadata_page(store, &workspace, &request)?);
        Ok(())
    })?;
    serde_json::to_value(result.ok_or_else(|| invalid_metadata("read section returned no page"))?)
        .map_err(|_| invalid_metadata("could not encode page"))
}

fn build_metadata_page(
    store: &dyn TaskStoreBackend,
    workspace: &str,
    request: &MetadataRequest,
) -> Result<MetadataPage, OrbitError> {
    let mut identity = blake3::Hasher::new();
    identity.update(&encode_metadata(&(workspace, &request.ids))?);
    let (items, total, remaining) = if let Some(ids) = &request.ids {
        // A bounded keyed selection, including missing ids in its identity;
        // no workspace scan even when every requested id is missing.
        let mut envelopes = store.task_envelopes_for_ids(ids)?;
        envelopes.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
        identity.update(&encode_metadata(&envelopes)?);
        let total = envelopes.len();
        envelopes.retain(|row| {
            request
                .cursor
                .as_ref()
                .is_none_or(|cursor| cursor.precedes(&MetadataCursor::of(row)))
        });
        let remaining = envelopes.len();
        envelopes.truncate(request.max_rows);
        (envelopes, total, remaining)
    } else {
        let keys = store
            .task_candidate_keys(
                &TaskListFilter {
                    scan_before: request
                        .cursor
                        .as_ref()
                        .map(|cursor| (cursor.created_at, cursor.id.clone())),
                    ..Default::default()
                },
                request.max_rows,
            )?
            .ok_or_else(|| invalid_metadata("bodyless index scan unavailable"))?;
        // The key query validated the index in this same read section. Stamps
        // detect changed/deleted/added tasks between pages, without bodies.
        let mut stamps = store.task_metadata_scan_stamps()?;
        stamps.sort_by(|a, b| a.id.cmp(&b.id));
        if stamps.len() != keys.total_without_cursor {
            return Err(invalid_metadata("scan population changed or is unsettled"));
        }
        for stamp in stamps {
            identity.update(&encode_metadata(&(
                stamp.id,
                stamp.status,
                stamp.updated_at,
            ))?);
        }
        let ids = keys.items.iter().map(|key| key.id.clone()).collect();
        let mut envelopes = store.task_envelopes_for_ids(&ids)?;
        envelopes.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
        if envelopes.len() != keys.items.len()
            || envelopes
                .iter()
                .zip(&keys.items)
                .any(|(row, key)| row.id != key.id || row.created_at != key.created_at)
        {
            return Err(invalid_metadata("selected metadata disappeared or changed"));
        }
        (envelopes, keys.total_without_cursor, keys.total)
    };
    let scan_id = identity.finalize().to_hex().to_string();
    if request
        .scan_id
        .as_ref()
        .is_some_and(|expected| *expected != scan_id)
    {
        return Err(invalid_metadata("scan identity changed"));
    }
    if items.len() > remaining || (items.is_empty() && remaining != 0) {
        return Err(invalid_metadata("incomplete index page"));
    }
    let mut page = MetadataPage {
        schema_version: 1,
        scan_id,
        total,
        remaining,
        complete: remaining == 0,
        next: None,
        items: Vec::new(),
    };
    for item in items {
        page.items.push(item);
        set_metadata_continuation(&mut page);
        if encode_metadata(&page)?.len() > request.max_bytes {
            page.items.pop();
            if page.items.is_empty() {
                return Err(invalid_metadata(
                    "one metadata row exceeds the response byte budget",
                ));
            }
            set_metadata_continuation(&mut page);
            break;
        }
    }
    if encode_metadata(&page)?.len() > request.max_bytes {
        return Err(invalid_metadata(
            "page header exceeds the response byte budget",
        ));
    }
    Ok(page)
}

fn set_metadata_continuation(page: &mut MetadataPage) {
    page.complete = page.items.len() == page.remaining;
    page.next = if page.complete {
        None
    } else {
        page.items.last().map(MetadataCursor::of)
    };
}

/// Collect only a complete validated scan; any error discards its prefix.
pub(crate) fn collect_owner_metadata(
    runtime: &OrbitRuntime,
    ids: Option<&BTreeSet<String>>,
) -> Result<Vec<Task>, OrbitError> {
    if let Some(ids) = ids {
        let mut tasks = Vec::new();
        // Bound request size and keyed owner work too. No full-corpus fallback.
        for chunk in ids.iter().collect::<Vec<_>>().chunks(MAX_METADATA_ROWS) {
            let selected = chunk.iter().map(|id| (*id).clone()).collect();
            tasks.extend(collect_metadata_scan(runtime, Some(selected))?);
        }
        return Ok(tasks);
    }
    collect_metadata_scan(runtime, None)
}

fn collect_metadata_scan(
    runtime: &OrbitRuntime,
    ids: Option<BTreeSet<String>>,
) -> Result<Vec<Task>, OrbitError> {
    let mut request = MetadataRequest {
        schema_version: 1,
        ids,
        scan_id: None,
        cursor: None,
        max_rows: MAX_METADATA_ROWS,
        max_bytes: MAX_METADATA_BYTES,
    };
    let mut tasks = Vec::new();
    let mut seen = BTreeSet::new();
    let mut expected_total = None;
    loop {
        let value: Value = runtime.read_owner_request(
            json!({ "_worker_read": "task_metadata", "metadata_page": &request }),
        )?;
        if encode_metadata(&value)?.len() > request.max_bytes {
            return Err(invalid_metadata("response exceeds the byte budget"));
        }
        let page: MetadataPage = serde_json::from_value(value)
            .map_err(|_| invalid_metadata("malformed page response"))?;
        if page.schema_version != 1
            || page.scan_id.len() != 64
            || !page.scan_id.bytes().all(|ch| ch.is_ascii_hexdigit())
            || request
                .scan_id
                .as_ref()
                .is_some_and(|id| *id != page.scan_id)
            || expected_total.is_some_and(|total| total != page.total)
            || page.total.checked_sub(tasks.len()) != Some(page.remaining)
            || page.items.len() > request.max_rows
            || page.items.len() > page.remaining
            || page.complete != (page.items.len() == page.remaining)
            || page.complete != page.next.is_none()
            || (!page.complete && page.items.is_empty())
            || request
                .ids
                .as_ref()
                .is_some_and(|ids| page.total > ids.len())
        {
            return Err(invalid_metadata("inconsistent or incomplete page"));
        }
        let mut previous = request.cursor.clone();
        for row in &page.items {
            row.validate()
                .map_err(|_| invalid_metadata("invalid envelope"))?;
            let cursor = MetadataCursor::of(row);
            if previous
                .as_ref()
                .is_some_and(|prev| !prev.precedes(&cursor))
                || !seen.insert(row.id.clone())
                || request
                    .ids
                    .as_ref()
                    .is_some_and(|ids| !ids.contains(&row.id))
            {
                return Err(invalid_metadata("unordered, repeated or unrequested row"));
            }
            previous = Some(cursor);
        }
        if !page.complete && page.next != previous {
            return Err(invalid_metadata("continuation does not follow the page"));
        }
        expected_total = Some(page.total);
        request.scan_id = Some(page.scan_id);
        request.cursor = page.next;
        tasks.extend(page.items.into_iter().map(metadata_task));
        if page.complete {
            return Ok(tasks);
        }
    }
}
