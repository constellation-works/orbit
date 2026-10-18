//! Dashboard polling and notification decisions, independent of the native shell.

use std::collections::HashSet;

use serde_json::Value;

/// How far back failed runs are counted: the dashboard's summary window.
const FAILED_SINCE: &str = "24h";
/// Items read per request. Review tasks must be read across every page because
/// creation order does not track when a task enters review.
const PAGE: usize = 200;
/// More new items than this in one read become a single notification.
const MAX_SEPARATE: usize = 3;

/// Counts the native shell can display after polling the dashboard.
pub enum PollSummary {
    /// No dashboard route answered.
    Unreadable,
    /// Available counts; `None` where that route failed.
    Live {
        /// Workspaces with an active drain.
        drains: Option<usize>,
        /// Failed runs in the dashboard's summary window.
        failed: Option<u64>,
    },
}

/// One item a notification can name.
struct Item {
    /// Workspace and run or task id, for comparing reads; never shown.
    key: (String, String),
    label: String,
    workspace: String,
}

/// What one read of a list returned.
struct Listing {
    items: Vec<Item>,
    total: Option<u64>,
    /// Workspaces whose items the server could not read.
    unavailable: HashSet<String>,
}

/// What the previous read of a list held.
struct Seen {
    keys: HashSet<(String, String)>,
    unavailable: HashSet<String>,
}

impl Seen {
    fn of(listing: &Listing) -> Self {
        Self {
            keys: listing.items.iter().map(|item| item.key.clone()).collect(),
            unavailable: listing.unavailable.clone(),
        }
    }

    /// Items not in the previous read. An item of a workspace that was
    /// unavailable then is old news returning, not a change.
    fn added<'a>(&self, listing: &'a Listing) -> Vec<&'a Item> {
        listing
            .items
            .iter()
            .filter(|item| {
                !self.keys.contains(&item.key) && !self.unavailable.contains(&item.key.0)
            })
            .collect()
    }
}

/// Stateful dashboard watcher. A first successful read establishes the baseline;
/// failed reads leave it intact. The native shell supplies loopback GETs and
/// native notifications, while other callers can supply their own adapters.
#[derive(Default)]
pub struct Watcher {
    failed_seen: Option<Seen>,
    review_seen: Option<Seen>,
}

impl Watcher {
    /// Poll the dashboard once. `get` reads a relative API route as JSON and
    /// returns `None` on failure; `notify` receives each announcement.
    pub fn poll(
        &mut self,
        mut get: impl FnMut(&str) -> Option<Value>,
        mut notify: impl FnMut(&str, &str),
    ) -> PollSummary {
        let drains = get("api/fleet/drains").map(|body| running_drains(&body));
        let failed = get(&format!(
            "api/job-runs/all?state=failed&since={FAILED_SINCE}&limit={PAGE}"
        ))
        .map(|body| failed_runs(&body));
        let review = review_snapshot(&mut get);

        let summary = if drains.is_none() && failed.is_none() && review.is_none() {
            PollSummary::Unreadable
        } else {
            PollSummary::Live {
                drains,
                failed: failed.as_ref().and_then(|listing| listing.total),
            }
        };
        if let Some(listing) = &failed {
            if let Some(seen) = &self.failed_seen {
                announce(
                    &seen.added(listing),
                    "Run failed",
                    "runs failed",
                    &mut notify,
                );
            }
            self.failed_seen = Some(Seen::of(listing));
        }
        if let Some(listing) = &review {
            if let Some(seen) = &self.review_seen {
                announce(
                    &seen.added(listing),
                    "Ready for review",
                    "tasks ready for review",
                    &mut notify,
                );
            }
            self.review_seen = Some(Seen::of(listing));
        }
        summary
    }
}

/// Workspaces whose drain is admitting work, pulling or winding down.
fn running_drains(body: &Value) -> usize {
    rows(body)
        .filter(|row| {
            row["phase"]
                .as_str()
                .is_some_and(|phase| !matches!(phase, "idle" | "unavailable"))
        })
        .count()
}

fn failed_runs(body: &Value) -> Listing {
    let items = rows(body)
        .filter_map(|run| {
            let task_title = run["tasks"]
                .as_array()
                .into_iter()
                .flatten()
                .find_map(|task| task["title"].as_str());
            Some(Item {
                key: (
                    run["workspace_id"].as_str()?.to_string(),
                    run["run_id"].as_str()?.to_string(),
                ),
                label: task_title.or_else(|| run["job_id"].as_str())?.to_string(),
                workspace: workspace_name(run),
            })
        })
        .collect();
    Listing {
        items,
        total: body["total"].as_u64(),
        unavailable: body["unavailable"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| entry["workspace_id"].as_str().map(str::to_string))
            .collect(),
    }
}

/// Only a complete cursor walk may replace the review baseline. A later page
/// failure must not turn returning page-two tasks into new review transitions.
fn review_snapshot(get: &mut impl FnMut(&str) -> Option<Value>) -> Option<Listing> {
    let route = format!("api/tasks/all?status=review&limit={PAGE}");
    let mut path = route.clone();
    let mut listing = Listing {
        items: Vec::new(),
        total: None,
        unavailable: HashSet::new(),
    };
    let mut cursors = HashSet::new();
    let mut keys = HashSet::new();
    loop {
        let body = get(&path)?;
        body["items"].as_array()?;
        let page = review_tasks(&body);
        listing.total = listing.total.or(page.total);
        listing.items.extend(
            page.items
                .into_iter()
                .filter(|item| keys.insert(item.key.clone())),
        );
        match &body["next_cursor"] {
            Value::Null => return Some(listing),
            Value::String(cursor) if !cursor.is_empty() && cursors.insert(cursor.clone()) => {
                // Cursors are opaque. Encode as a query value and preserve the
                // filters and page size to which the server binds each cursor.
                let query = url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("cursor", cursor)
                    .finish();
                path = format!("{route}&{query}");
            }
            // A malformed or repeated cursor cannot establish a full snapshot.
            _ => return None,
        }
    }
}

fn review_tasks(body: &Value) -> Listing {
    let items = rows(body)
        .filter_map(|task| {
            Some(Item {
                key: (
                    task["workspace_id"].as_str()?.to_string(),
                    task["id"].as_str()?.to_string(),
                ),
                label: task["title"].as_str()?.to_string(),
                workspace: workspace_name(task),
            })
        })
        .collect();
    Listing {
        items,
        total: body["total"].as_u64(),
        unavailable: HashSet::new(),
    }
}

fn rows(body: &Value) -> impl Iterator<Item = &Value> {
    body["items"].as_array().into_iter().flatten()
}

fn workspace_name(row: &Value) -> String {
    row["workspace_name"]
        .as_str()
        .or_else(|| row["workspace_id"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// One notification per new item, or one for the lot when there are many.
fn announce(added: &[&Item], title: &str, plural: &str, notify: &mut impl FnMut(&str, &str)) {
    if added.len() > MAX_SEPARATE {
        let mut workspaces: Vec<&str> = added.iter().map(|item| item.workspace.as_str()).collect();
        workspaces.sort_unstable();
        workspaces.dedup();
        notify(
            &format!("{} {plural}", added.len()),
            &format!("In {}", workspaces.join(", ")),
        );
        return;
    }
    for item in added {
        notify(title, &format!("{} · {}", item.label, item.workspace));
    }
}
