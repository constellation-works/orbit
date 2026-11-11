//! Fleet notices: short, expiring, attributed announcements every agent and
//! operator can read, stored in the owner's store.
//!
//! This module owns every decision: who is posting (`authority`), what a
//! post may set and whether it creates, updates or corroborates (`post`),
//! the effective state a read reports, and the audit row each write and each
//! refusal leaves (target type `notice`).

mod authority;
pub mod limits;
mod post;
#[cfg(feature = "test-support")]
pub(crate) mod test_support;

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_common::{NoticeRefusalCode, OrbitError};
use orbit_types::record::{AuthorClass, EffectiveStatus, Notice, NoticeKind, NoticeScope};
use serde::Serialize;

pub use authority::NoticeCaller;
pub use post::{NoticePostRawFields, NoticePostRequest, NoticePostResult};

/// Which notice a close names.
#[derive(Debug, Clone)]
pub enum NoticeSelector {
    Id(String),
    /// The open notice of the home workspace holding this key.
    DedupeKey(String),
}

/// What a close did.
#[derive(Debug, Clone, Serialize)]
pub struct NoticeCloseResult {
    pub notice: Notice,
    /// The notice was already closed or expired; nothing was written.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub already_inactive: bool,
}

/// Effective-state filter for a listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NoticeStatusFilter {
    #[default]
    Open,
    Closed,
    Expired,
    All,
}

impl NoticeStatusFilter {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "closed" => Some(Self::Closed),
            "expired" => Some(Self::Expired),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    fn admits(self, status: EffectiveStatus) -> bool {
        match self {
            Self::Open => status == EffectiveStatus::Open,
            Self::Closed => status == EffectiveStatus::Closed,
            Self::Expired => status == EffectiveStatus::Expired,
            Self::All => true,
        }
    }
}

/// A listing's filters.
#[derive(Debug, Clone)]
pub struct NoticeListFilter {
    pub status: NoticeStatusFilter,
    pub include_global: bool,
    /// Read every workspace homed on this owner, not only the bound one:
    /// the owner-local aggregate a federated fan-out issues per owner.
    pub all_workspaces: bool,
    pub scope: Option<NoticeScope>,
    pub kinds: Vec<NoticeKind>,
    pub linked_task: Option<String>,
    pub author_class: Option<AuthorClass>,
    pub host: Option<String>,
    /// `None` returns every match; an aggregating caller applies its own
    /// limit once, after merging the owners' answers.
    pub limit: Option<usize>,
}

impl Default for NoticeListFilter {
    fn default() -> Self {
        Self {
            status: NoticeStatusFilter::Open,
            include_global: true,
            all_workspaces: false,
            scope: None,
            kinds: Vec::new(),
            linked_task: None,
            author_class: None,
            host: None,
            limit: Some(50),
        }
    }
}

/// A listing: the matching notices and the homes that could not be read.
#[derive(Debug, Clone, Serialize)]
pub struct NoticeListing {
    pub notices: Vec<Notice>,
    pub degraded: Vec<String>,
}

/// One history entry of a notice, shown to the operator.
#[derive(Debug, Clone, Serialize)]
pub struct NoticeHistoryEntry {
    pub at: DateTime<Utc>,
    pub action: String,
    pub poster_class: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poster_crew: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poster_task_id: Option<String>,
    pub detail: serde_json::Value,
}

/// One notice, with its history when the caller holds the operator
/// capability.
#[derive(Debug, Clone, Serialize)]
pub struct NoticeView {
    pub notice: Notice,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<Vec<NoticeHistoryEntry>>,
}

mod audit;
mod close;
mod read;
pub(crate) use audit::{NoticeAudit, notice_row, parse_notice_row};
use close::linked_done_at;

fn refuse(code: NoticeRefusalCode, message: impl Into<String>) -> OrbitError {
    OrbitError::notice_refused(code, message)
}

/// Done linked tasks by id, with the time each was last updated.
pub(crate) type LinkedDone = BTreeMap<String, DateTime<Utc>>;
