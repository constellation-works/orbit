//! Opaque, scope-bound cursors for dashboard task pagination.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use orbit_core::application::task::TaskListFilter;
use orbit_core::{DEFAULT_TASK_LIST_LIMIT, TaskStatus, TaskType};
use serde::{Deserialize, Serialize};
use serde_json::json;

const CURSOR_VERSION: u8 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct TaskCursor {
    version: u8,
    scope: String,
    filters: String,
    pub(super) created_at: DateTime<Utc>,
    pub(super) id: String,
    pub(super) offset: usize,
    checksum: String,
}

pub(super) struct TaskPageQuery {
    statuses: Vec<TaskStatus>,
    no_statuses: bool,
    tags: Vec<String>,
    task_type: Option<TaskType>,
    search: Option<String>,
    limit: usize,
    raw_cursor: Option<String>,
    cursor: Option<TaskCursor>,
}

impl Default for TaskPageQuery {
    fn default() -> Self {
        Self {
            statuses: Vec::new(),
            no_statuses: false,
            tags: Vec::new(),
            task_type: None,
            search: None,
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
                "q" | "search" => {
                    query.search =
                        Some(value.trim().to_lowercase()).filter(|value| !value.is_empty());
                }
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
        TaskListFilter {
            scan_before: self
                .cursor
                .as_ref()
                .map(|cursor| (cursor.created_at, cursor.id.clone())),
            search: self.search.clone(),
            statuses: if self.no_statuses {
                Some(Vec::new())
            } else {
                (!self.statuses.is_empty()).then(|| self.statuses.clone())
            },
            task_type: self.task_type,
            tags: self.tags.clone(),
            ..Default::default()
        }
    }

    pub(super) fn count_filter(&self) -> TaskListFilter {
        let mut filter = self.filter();
        filter.scan_before = None;
        filter
    }

    pub(super) fn limit(&self) -> usize {
        self.limit
    }

    pub(super) fn offset(&self) -> usize {
        self.cursor.as_ref().map_or(0, |cursor| cursor.offset)
    }

    pub(super) fn next_cursor(
        &self,
        scope: &str,
        created_at: DateTime<Utc>,
        id: String,
        offset: usize,
    ) -> Result<String, serde_json::Error> {
        TaskCursor::new(scope, &self.filter_key(), created_at, id, offset).encode()
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
        json!({
            "statuses": statuses,
            "no_statuses": self.no_statuses,
            "tags": tags,
            "type": self.task_type.map(|value| value.to_string()),
            "search": self.search,
            "limit": self.limit,
        })
        .to_string()
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
    Ok(value)
}

impl TaskCursor {
    pub(super) fn new(
        scope: &str,
        filters: &str,
        created_at: DateTime<Utc>,
        id: String,
        offset: usize,
    ) -> Self {
        let mut cursor = Self {
            version: CURSOR_VERSION,
            scope: scope.to_string(),
            filters: filters.to_string(),
            created_at,
            id,
            offset,
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
                "task cursor does not match the active status, tag, type, or search filters"
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
        blake3::hash(
            format!(
                "{}\0{}\0{}\0{}\0{}\0{}",
                self.version,
                self.scope,
                self.filters,
                self.created_at.to_rfc3339(),
                self.id,
                self.offset,
            )
            .as_bytes(),
        )
        .to_hex()
        .to_string()
    }
}
