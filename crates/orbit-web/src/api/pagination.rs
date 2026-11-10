//! Opaque, scope-bound cursors for dashboard task pagination.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use orbit_core::application::task::TaskListFilter;
use orbit_core::{DEFAULT_TASK_LIST_LIMIT, TaskStatus, TaskType};
use orbit_types::task::TaskOrigin;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::HISTORY_MAX_LIMIT;

const CURSOR_VERSION: u8 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct TaskCursor {
    version: u8,
    scope: String,
    filters: String,
    pub(super) created_at: DateTime<Utc>,
    pub(super) id: String,
    pub(super) offset: usize,
    /// The workspace the last row belonged to, for the aggregate listing,
    /// which orders workspace by workspace: earlier workspaces are spent,
    /// this one resumes after `created_at`/`id`, later ones start fresh.
    /// Absent on single-workspace cursors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) workspace_id: Option<String>,
    checksum: String,
}

pub(super) struct TaskPageQuery {
    pub(super) workspaces: Vec<String>,
    statuses: Vec<TaskStatus>,
    no_statuses: bool,
    pilot_warnings: bool,
    tags: Vec<String>,
    task_type: Option<TaskType>,
    origin: Option<TaskOrigin>,
    search: Option<String>,
    detail_id: Option<String>,
    detail_runs: bool,
    limit: usize,
    raw_cursor: Option<String>,
    cursor: Option<TaskCursor>,
}

impl Default for TaskPageQuery {
    fn default() -> Self {
        Self {
            workspaces: Vec::new(),
            statuses: Vec::new(),
            no_statuses: false,
            pilot_warnings: false,
            tags: Vec::new(),
            task_type: None,
            origin: None,
            search: None,
            detail_id: None,
            detail_runs: false,
            limit: DEFAULT_TASK_LIST_LIMIT,
            raw_cursor: None,
            cursor: None,
        }
    }
}

impl TaskPageQuery {
    pub(super) fn parse(raw_query: Option<&str>) -> Result<Self, String> {
        let mut query = Self::default();
        for (key, value) in url::form_urlencoded::parse(raw_query.unwrap_or_default().as_bytes()) {
            match key.as_ref() {
                "workspaces" => query.workspaces.push(value.into_owned()),
                "pilot" => query.pilot_warnings = value == "warnings",
                "status" => query.parse_statuses(&value)?,
                "tag" | "tags" => query.tags.extend(split_values(&value).map(str::to_string)),
                "type" | "task_type" => {
                    if let Some(raw) = split_values(&value).next_back() {
                        query.task_type =
                            Some(raw.to_ascii_lowercase().parse::<TaskType>().map_err(
                                |error| format!("invalid `type` value `{raw}`: {error}"),
                            )?);
                    }
                }
                "origin" => {
                    if let Some(raw) = split_values(&value).next_back() {
                        query.origin =
                            Some(raw.to_ascii_lowercase().parse::<TaskOrigin>().map_err(
                                |error| format!("invalid `origin` value `{raw}`: {error}"),
                            )?);
                    }
                }
                "q" | "search" => {
                    query.search =
                        Some(value.trim().to_lowercase()).filter(|value| !value.is_empty());
                }
                "detail" => {
                    query.detail_id =
                        Some(value.trim().to_string()).filter(|value| !value.is_empty());
                }
                "detail_runs" => query.detail_runs = value == "true",
                "limit" => {
                    if let Some(raw) = split_values(&value).next_back() {
                        query.limit = parse_limit(raw)?;
                    }
                }
                "cursor" => {
                    query.raw_cursor = Some(value.into_owned()).filter(|value| !value.is_empty())
                }
                _ => {}
            }
        }
        Ok(query)
    }

    pub(super) fn bind_cursor(&mut self, scope: &str) -> Result<(), String> {
        if let Some(raw_cursor) = self.raw_cursor.take() {
            self.cursor = Some(TaskCursor::decode(&raw_cursor, scope, &self.filter_key())?);
        }
        Ok(())
    }

    pub(super) fn filter(&self) -> TaskListFilter {
        self.filter_scanning_before(
            self.cursor
                .as_ref()
                .map(|cursor| (cursor.created_at, cursor.id.clone())),
        )
    }

    /// [`Self::filter`] with an explicit resume point instead of the cursor's,
    /// for a listing that applies the cursor to one source only.
    pub(super) fn filter_scanning_before(
        &self,
        scan_before: Option<(DateTime<Utc>, String)>,
    ) -> TaskListFilter {
        TaskListFilter {
            scan_before,
            search: self.search.clone(),
            statuses: if self.no_statuses {
                Some(Vec::new())
            } else {
                (!self.statuses.is_empty()).then(|| self.statuses.clone())
            },
            task_type: self.task_type,
            origin: self.origin,
            tags: self.tags.clone(),
            ..Default::default()
        }
    }

    pub(super) fn pilot_warnings(&self) -> bool {
        self.pilot_warnings
    }

    pub(super) fn detail_id(&self) -> Option<&str> {
        self.detail_id.as_deref()
    }

    pub(super) fn include_detail_runs(&self) -> bool {
        self.detail_runs
    }

    pub(super) fn limit(&self) -> usize {
        self.limit
    }

    pub(super) fn offset(&self) -> usize {
        self.cursor.as_ref().map_or(0, |cursor| cursor.offset)
    }

    pub(super) fn cursor(&self) -> Option<&TaskCursor> {
        self.cursor.as_ref()
    }

    pub(super) fn next_cursor(
        &self,
        scope: &str,
        created_at: DateTime<Utc>,
        id: String,
        offset: usize,
    ) -> Result<String, serde_json::Error> {
        TaskCursor::new(scope, &self.filter_key(), created_at, id, offset, None).encode()
    }

    /// [`Self::next_cursor`] for the aggregate listing, naming the workspace
    /// the page's last row belongs to.
    pub(super) fn next_workspace_cursor(
        &self,
        scope: &str,
        workspace_id: &str,
        created_at: DateTime<Utc>,
        id: String,
        offset: usize,
    ) -> Result<String, serde_json::Error> {
        TaskCursor::new(
            scope,
            &self.filter_key(),
            created_at,
            id,
            offset,
            Some(workspace_id.to_string()),
        )
        .encode()
    }

    fn parse_statuses(&mut self, value: &str) -> Result<(), String> {
        for raw in split_values(value) {
            if raw.eq_ignore_ascii_case("none") {
                self.no_statuses = true;
                self.statuses.clear();
                continue;
            }
            let status = raw
                .to_ascii_lowercase()
                .parse::<TaskStatus>()
                .map_err(|error| format!("invalid `status` value `{raw}`: {error}"))?;
            if !self.statuses.contains(&status) {
                self.statuses.push(status);
            }
        }
        Ok(())
    }

    fn filter_key(&self) -> String {
        let mut statuses = self
            .statuses
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let mut tags = self
            .tags
            .iter()
            .map(|tag| tag.trim().to_lowercase())
            .collect::<Vec<_>>();
        statuses.sort();
        tags.sort();
        let mut key = json!({
            "statuses": statuses,
            "no_statuses": self.no_statuses,
            "tags": tags,
            "type": self.task_type.map(|value| value.to_string()),
            "search": self.search,
            "limit": self.limit,
        });
        // Keyed only when set, so a cursor issued before the origin filter
        // existed still binds to the same unfiltered listing.
        if self.pilot_warnings {
            key["pilot_warnings"] = json!(true);
        }
        if let Some(origin) = self.origin {
            key["origin"] = json!(origin.as_str());
        }
        if !self.workspaces.is_empty() {
            let mut workspaces = self.workspaces.clone();
            workspaces.sort();
            workspaces.dedup();
            key["workspaces"] = json!(workspaces);
        }
        key.to_string()
    }
}

fn split_values(value: &str) -> impl DoubleEndedIterator<Item = &str> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
}

fn parse_limit(raw: &str) -> Result<usize, String> {
    let value = raw
        .parse::<usize>()
        .map_err(|_| format!("invalid `limit` value `{raw}` (expected a positive integer)"))?;
    if value == 0 {
        return Err("`limit` must be at least 1".to_string());
    }
    // Clamp here, before `filter_key` embeds the limit in the cursor, so an
    // oversized request cannot force an unbounded scan and cursors minted for
    // it stay valid for the clamped page size.
    Ok(value.min(HISTORY_MAX_LIMIT))
}

impl TaskCursor {
    pub(super) fn new(
        scope: &str,
        filters: &str,
        created_at: DateTime<Utc>,
        id: String,
        offset: usize,
        workspace_id: Option<String>,
    ) -> Self {
        let mut cursor = Self {
            version: CURSOR_VERSION,
            scope: scope.to_string(),
            filters: filters.to_string(),
            created_at,
            id,
            offset,
            workspace_id,
            checksum: String::new(),
        };
        cursor.checksum = cursor.expected_checksum();
        cursor
    }

    pub(super) fn encode(&self) -> Result<String, serde_json::Error> {
        serde_json::to_vec(self).map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
    }

    pub(super) fn decode(raw: &str, scope: &str, filters: &str) -> Result<Self, String> {
        if raw.len() > 4096 {
            return Err("invalid task cursor: payload is too large".to_string());
        }
        let bytes = URL_SAFE_NO_PAD.decode(raw).map_err(|_| {
            "invalid task cursor: expected an opaque cursor returned by this endpoint".to_string()
        })?;
        let cursor: Self = serde_json::from_slice(&bytes)
            .map_err(|_| "invalid task cursor: payload is malformed".to_string())?;
        if cursor.version != CURSOR_VERSION {
            return Err(format!(
                "invalid task cursor version {}; expected {CURSOR_VERSION}",
                cursor.version
            ));
        }
        if cursor.scope != scope {
            return Err("task cursor belongs to a different workspace or endpoint".to_string());
        }
        if cursor.filters != filters {
            return Err(
                "task cursor does not match the active status, tag, type, origin, or search filters"
                    .to_string(),
            );
        }
        if cursor.id.trim().is_empty() {
            return Err("invalid task cursor: task id is empty".to_string());
        }
        if cursor.checksum != cursor.expected_checksum() {
            return Err("invalid task cursor: integrity check failed".to_string());
        }
        Ok(cursor)
    }

    fn expected_checksum(&self) -> String {
        let mut material = format!(
            "{}\0{}\0{}\0{}\0{}\0{}",
            self.version,
            self.scope,
            self.filters,
            self.created_at.to_rfc3339(),
            self.id,
            self.offset,
        );
        // Appended only when set, so a single-workspace cursor keeps the
        // checksum it had before aggregate cursors named a workspace.
        if let Some(workspace_id) = &self.workspace_id {
            material.push('\0');
            material.push_str(workspace_id);
        }
        blake3::hash(material.as_bytes()).to_hex().to_string()
    }
}
