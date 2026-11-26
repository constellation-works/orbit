//! Resident wake observations and their legacy grant and digest encoding.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_types::identity::OrbitId;
use orbit_types::task::{DecisionRequest, DecisionRequestState, TaskStatus};
use orbit_types::workflow::ResidentWakeItems;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ResidentWakeItemKey {
    Mandate,
    Authority,
    Assignment,
    Watch,
    Comments,
    Notices,
    Notice(OrbitId),
    Task(OrbitId),
    Unknown(String),
}

impl ResidentWakeItemKey {
    fn from_legacy(key: String) -> Self {
        match key.as_str() {
            "mandate" => Self::Mandate,
            "authority" => Self::Authority,
            "assignment" => Self::Assignment,
            "watch" => Self::Watch,
            "comments" => Self::Comments,
            "notices" => Self::Notices,
            _ => {
                if let Some(id) = key.strip_prefix("task:") {
                    Self::Task(id.to_owned())
                } else if let Some(id) = key.strip_prefix("notice:") {
                    Self::Notice(id.to_owned())
                } else {
                    Self::Unknown(key)
                }
            }
        }
    }

    fn to_legacy(&self) -> String {
        match self {
            Self::Mandate => "mandate".into(),
            Self::Authority => "authority".into(),
            Self::Assignment => "assignment".into(),
            Self::Watch => "watch".into(),
            Self::Comments => "comments".into(),
            Self::Notices => "notices".into(),
            Self::Notice(id) => format!("notice:{id}"),
            Self::Task(id) => format!("task:{id}"),
            Self::Unknown(key) => key.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ResidentTaskWakeClass {
    Active,
    Blocked,
    Review,
    Someday,
    Done,
    Archived,
    Rejected,
    Unknown(String),
}

impl From<TaskStatus> for ResidentTaskWakeClass {
    fn from(status: TaskStatus) -> Self {
        match status {
            TaskStatus::Proposed | TaskStatus::Backlog | TaskStatus::InProgress => Self::Active,
            TaskStatus::Blocked => Self::Blocked,
            TaskStatus::Review => Self::Review,
            TaskStatus::Someday => Self::Someday,
            TaskStatus::Done => Self::Done,
            TaskStatus::Archived => Self::Archived,
            TaskStatus::Rejected => Self::Rejected,
        }
    }
}

impl ResidentTaskWakeClass {
    fn from_legacy(class: &str) -> Self {
        match class {
            "active" => Self::Active,
            "blocked" => Self::Blocked,
            "review" => Self::Review,
            "someday" => Self::Someday,
            "done" => Self::Done,
            "archived" => Self::Archived,
            "rejected" => Self::Rejected,
            _ => Self::Unknown(class.into()),
        }
    }

    fn as_legacy(&self) -> &str {
        match self {
            Self::Active => "active",
            Self::Blocked => "blocked",
            Self::Review => "review",
            Self::Someday => "someday",
            Self::Done => "done",
            Self::Archived => "archived",
            Self::Rejected => "rejected",
            Self::Unknown(class) => class,
        }
    }

    pub(super) fn is_closed(&self) -> bool {
        matches!(self, Self::Done | Self::Archived | Self::Rejected)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResidentTaskWakeItem {
    class: ResidentTaskWakeClass,
    // The scheduler compares suffix bytes exactly. Historical suffixes can
    // contain arbitrary text, including repeated or trailing spaces.
    suffix: String,
}

impl ResidentTaskWakeItem {
    pub(super) fn new(
        status: TaskStatus,
        pr_status: Option<&str>,
        updated_at: Option<DateTime<Utc>>,
        requests: &[DecisionRequest],
    ) -> Self {
        let mut suffix = String::new();
        if let Some(pr_status) = pr_status.filter(|pr| !pr.is_empty()) {
            suffix.push_str(&format!(" pr:{pr_status}"));
        }
        if let Some(updated_at) = updated_at {
            suffix.push_str(&format!(" at:{}", updated_at.to_rfc3339()));
        }
        let settled = settled_decisions(requests);
        if !settled.is_empty() {
            suffix.push(' ');
            suffix.push_str(&settled);
        }
        Self {
            class: status.into(),
            suffix,
        }
    }

    fn from_legacy(value: String) -> Self {
        let (class, suffix) = value.split_at(value.find(' ').unwrap_or(value.len()));
        Self {
            class: ResidentTaskWakeClass::from_legacy(class),
            suffix: suffix.into(),
        }
    }

    pub(super) fn to_legacy(&self) -> String {
        format!("{}{}", self.class.as_legacy(), self.suffix)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResidentWakeItem {
    Task(ResidentTaskWakeItem),
    Opaque(String),
}

/// What a cycle reacts to, including the retained baseline of earlier cycles.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ResidentWakeObservation(BTreeMap<ResidentWakeItemKey, ResidentWakeItem>);

impl ResidentWakeObservation {
    pub(super) fn from_legacy(items: ResidentWakeItems) -> Self {
        Self(
            items
                .into_iter()
                .map(|(key, value)| {
                    let key = ResidentWakeItemKey::from_legacy(key);
                    let item = if matches!(key, ResidentWakeItemKey::Task(_)) {
                        ResidentWakeItem::Task(ResidentTaskWakeItem::from_legacy(value))
                    } else {
                        ResidentWakeItem::Opaque(value)
                    };
                    (key, item)
                })
                .collect(),
        )
    }

    pub(super) fn to_legacy(&self) -> ResidentWakeItems {
        self.0
            .iter()
            .map(|(key, item)| {
                let value = match item {
                    ResidentWakeItem::Task(task) => task.to_legacy(),
                    ResidentWakeItem::Opaque(value) => value.clone(),
                };
                (key.to_legacy(), value)
            })
            .collect()
    }

    pub(super) fn insert(&mut self, key: ResidentWakeItemKey, value: String) {
        self.0.insert(key, ResidentWakeItem::Opaque(value));
    }

    pub(super) fn insert_task(&mut self, id: OrbitId, task: ResidentTaskWakeItem) {
        self.0
            .insert(ResidentWakeItemKey::Task(id), ResidentWakeItem::Task(task));
    }

    pub(super) fn overlay(&mut self, settled: Self) {
        self.0.extend(settled.0);
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn tasks(&self) -> impl Iterator<Item = (&str, &ResidentTaskWakeClass)> {
        self.0.iter().filter_map(|(key, item)| match (key, item) {
            (ResidentWakeItemKey::Task(id), ResidentWakeItem::Task(task)) => {
                Some((id.as_str(), &task.class))
            }
            _ => None,
        })
    }

    /// Existing item changes always count. Tasks joining or leaving the set
    /// of subjects count only when a new one is blocked or in review.
    pub(super) fn changed_since(&self, baseline: &Self) -> bool {
        self.0.iter().any(|(key, item)| match baseline.0.get(key) {
            Some(before) => before != item,
            None => match item {
                ResidentWakeItem::Task(task) => matches!(
                    task.class,
                    ResidentTaskWakeClass::Blocked | ResidentTaskWakeClass::Review
                ),
                ResidentWakeItem::Opaque(_) => true,
            },
        }) || baseline
            .0
            .keys()
            .any(|key| !matches!(key, ResidentWakeItemKey::Task(_)) && !self.0.contains_key(key))
    }
}

/// Open decisions are already visible in the brief. Only a settlement wakes.
pub(super) fn settled_decisions(requests: &[DecisionRequest]) -> String {
    let mut settled: Vec<String> = requests
        .iter()
        .filter(|request| request.state != DecisionRequestState::Open)
        .map(|request| format!("{}={}", request.id, request.state.as_str()))
        .collect();
    settled.sort();
    if settled.is_empty() {
        String::new()
    } else {
        format!("decisions:{}", settled.join(","))
    }
}
