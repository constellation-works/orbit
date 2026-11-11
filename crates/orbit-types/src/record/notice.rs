use std::cmp::Ordering;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Where a notice applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeScope {
    /// The home workspace only.
    Workspace,
    /// Every workspace whose readers reach the home owner.
    Global,
}

impl NoticeScope {
    pub const NAMES: &'static str = "workspace or global";

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Global => "global",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "workspace" => Some(Self::Workspace),
            "global" => Some(Self::Global),
            _ => None,
        }
    }
}

/// What a notice is about. The variant order is the listing order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    /// Something is broken now.
    Incident,
    /// Planned work others should hold off or plan around.
    Maintenance,
    /// Anything else worth knowing.
    Info,
}

impl NoticeKind {
    pub const ALL: [Self; 3] = [Self::Incident, Self::Maintenance, Self::Info];
    pub const NAMES: &'static str = "incident, maintenance or info";

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Incident => "incident",
            Self::Maintenance => "maintenance",
            Self::Info => "info",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }
}

/// Who posted a notice, derived from the caller's session, never from input.
/// `System` appears only as the closer of a linked-task closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorClass {
    Operator,
    Orchestrator,
    Agent,
    System,
}

impl AuthorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Orchestrator => "orchestrator",
            Self::Agent => "agent",
            Self::System => "system",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "operator" => Some(Self::Operator),
            "orchestrator" => Some(Self::Orchestrator),
            "agent" => Some(Self::Agent),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

/// A notice's effective state, computed on every read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveStatus {
    Open,
    Closed,
    Expired,
}

impl EffectiveStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Expired => "expired",
        }
    }
}

/// What a post did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostOutcome {
    /// No open notice held the key: a new notice.
    Created,
    /// The poster may edit the open notice holding the key.
    Updated,
    /// The poster may not edit it: only its reports changed.
    Corroborated,
}

impl PostOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Updated => "updated",
            Self::Corroborated => "corroborated",
        }
    }
}

/// Why a closed notice closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeCloseCause {
    Manual,
    LinkedTaskDone,
}

/// The home workspace of a notice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeWorkspace {
    pub id: String,
    pub name: String,
}

/// A notice's author or closer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeAuthor {
    pub class: AuthorClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
}

impl NoticeAuthor {
    /// The closer recorded for a linked-task closure.
    pub fn system() -> Self {
        Self {
            class: AuthorClass::System,
            crew: None,
            model: None,
            task_id: None,
            run_id: None,
            machine_id: None,
        }
    }
}

/// The task a notice is about, usually its fix. `status` is filled on read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeLinkedTask {
    pub id: String,
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// One evidence reference. Stored and shown, never dereferenced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeEvidence {
    #[serde(rename = "type")]
    pub kind: String,
    pub value: String,
}

impl NoticeEvidence {
    pub const TYPES: [&'static str; 6] =
        ["task", "run", "friction", "pull_request", "url", "commit"];
}

/// Posts that matched a notice while it was open, including the first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeReports {
    pub count: u32,
    pub last_at: DateTime<Utc>,
}

/// How a notice closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeClosed {
    pub at: DateTime<Utc>,
    pub by: NoticeAuthor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub cause: NoticeCloseCause,
}

/// One fleet notice: the stored fields plus the read-time `status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub id: String,
    pub workspace: NoticeWorkspace,
    pub scope: NoticeScope,
    pub kind: NoticeKind,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub dedupe_key: String,
    pub author: NoticeAuthor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_task: Option<NoticeLinkedTask>,
    #[serde(default)]
    pub close_on_linked_done: bool,
    #[serde(default)]
    pub evidence: Vec<NoticeEvidence>,
    #[serde(default)]
    pub hosts: Vec<String>,
    pub expires_at: DateTime<Utc>,
    pub opened_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub revision: u32,
    pub reports: NoticeReports,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<NoticeClosed>,
    /// Effective state; never stored as `expired`.
    pub status: EffectiveStatus,
}

/// The fields that place a notice in a listing. Its `Ord` is the notice
/// list order every surface uses: kind (incident, maintenance, info), then
/// the newest `updated_at` first, then id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoticeListOrderKey<'a> {
    pub kind: NoticeKind,
    pub updated_at: DateTime<Utc>,
    pub id: &'a str,
}

impl Ord for NoticeListOrderKey<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.kind
            .cmp(&other.kind)
            .then_with(|| other.updated_at.cmp(&self.updated_at))
            .then_with(|| self.id.cmp(other.id))
    }
}

impl PartialOrd for NoticeListOrderKey<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Notice {
    /// This notice's place in the notice list order.
    pub fn list_order_key(&self) -> NoticeListOrderKey<'_> {
        NoticeListOrderKey {
            kind: self.kind,
            updated_at: self.updated_at,
            id: &self.id,
        }
    }

    /// Compare two notices in the notice list order.
    pub fn cmp_list_order(&self, other: &Self) -> Ordering {
        self.list_order_key().cmp(&other.list_order_key())
    }

    /// The effective state at `now`, given the linked task's status when it
    /// was readable. Closure by a done linked task is reported in `closed`.
    pub fn resolve_effective_status(
        &mut self,
        now: DateTime<Utc>,
        linked_done_at: Option<DateTime<Utc>>,
    ) {
        if self.closed.is_some() {
            self.status = EffectiveStatus::Closed;
            return;
        }
        if self.linked_task.is_some()
            && self.close_on_linked_done
            && let Some(at) = linked_done_at
        {
            self.closed = Some(NoticeClosed {
                at,
                by: NoticeAuthor::system(),
                reason: None,
                cause: NoticeCloseCause::LinkedTaskDone,
            });
            self.status = EffectiveStatus::Closed;
            return;
        }
        self.status = if now >= self.expires_at {
            EffectiveStatus::Expired
        } else {
            EffectiveStatus::Open
        };
    }
}
