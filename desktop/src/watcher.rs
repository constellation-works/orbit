//! Dashboard polling and notification decisions, independent of the native shell.
//!
//! Each poll reads `api/hosts` once, without probing, then the drain, failed-run
//! and review routes for the serving host and every other registered host that
//! is not already known to be unreachable. Remote routes go through
//! `api/on/<host>/…`. Items are keyed by host, workspace and id. When more than
//! one host is registered, notification text names the host. Drain and failure
//! counts are sums across the hosts that answered. A host that does not answer
//! keeps the baseline from its last answer, so those items are not announced
//! again when it returns, and the missing answer makes that count unavailable
//! instead of adding zero.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

/// How far back failed runs are counted: the dashboard's summary window.
const FAILED_SINCE: &str = "24h";
/// Items read per request. Review tasks must be read across every page because
/// creation order does not track when a task enters review.
const PAGE: usize = 200;
/// More new items than this in one read become a single notification.
const MAX_SEPARATE: usize = 3;

/// Counts the native shell can display after polling the dashboard.
#[derive(Debug, PartialEq, Eq)]
pub enum PollSummary {
    /// No dashboard route answered.
    Unreadable,
    /// Available counts; `None` where that route failed or a host did not answer.
    Live {
        /// Workspaces with an active drain, summed across hosts.
        drains: Option<usize>,
        /// Failed runs in the dashboard's summary window, summed across hosts.
        failed: Option<u64>,
    },
}

/// One registered host to poll. The serving host uses the local `/api` routes.
#[derive(Clone)]
struct PollHost {
    name: String,
    local: bool,
    /// `Some(false)` when the host list says this host did not answer.
    /// `None` means the list was not probed; the forward itself is the check.
    reachable: Option<bool>,
}

/// One item a notification can name.
#[derive(Clone)]
struct Item {
    /// Host, workspace and run or task id, for comparing reads; never shown.
    key: ItemKey,
    label: String,
    workspace: String,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ItemKey {
    host: String,
    workspace: String,
    id: String,
}

/// What one read of a list returned.
struct Listing {
    items: Vec<Item>,
    total: Option<u64>,
    /// Host and workspace pairs whose items the server could not read.
    unavailable: HashSet<(String, String)>,
}

/// What the previous read of one host's list held.
struct Seen {
    keys: HashSet<ItemKey>,
    unavailable: HashSet<(String, String)>,
}

impl Seen {
    fn of(listing: &Listing) -> Self {
        Self {
            keys: listing.items.iter().map(|item| item.key.clone()).collect(),
            unavailable: listing.unavailable.clone(),
        }
    }

    /// Failed runs not in the previous read. An item of a workspace that was
    /// unavailable then is old news returning, not a new failure. That also
    /// hides a failure first seen after recovery; review polling does not use
    /// this rule, because a task that enters review during the outage must
    /// still be announced.
    fn added<'a>(&self, listing: &'a Listing) -> Vec<&'a Item> {
        listing
            .items
            .iter()
            .filter(|item| {
                !self.keys.contains(&item.key) && !self.unavailable.contains(&scope(&item.key))
            })
            .collect()
    }

    /// Remember this review read. Keys the server could not re-read stay
    /// remembered, so a task that never left review is not announced when its
    /// workspace returns. A key that was never seen is still new.
    fn remember(&self, listing: &Listing) -> Self {
        let mut keys: HashSet<ItemKey> =
            listing.items.iter().map(|item| item.key.clone()).collect();
        for key in &self.keys {
            if listing.unavailable.contains(&scope(key)) {
                keys.insert(key.clone());
            }
        }
        Self {
            keys,
            unavailable: listing.unavailable.clone(),
        }
    }
}

fn scope(key: &ItemKey) -> (String, String) {
    (key.host.clone(), key.workspace.clone())
}

/// The last successful read of one host. A host that returns nothing keeps
/// these baselines, so its items are not announced again when it returns.
#[derive(Default)]
struct HostMemory {
    failed_seen: Option<Seen>,
    review_seen: Option<Seen>,
}

/// One host's answers for this poll. `None` means that route did not answer.
struct HostRead {
    name: String,
    drains: Option<usize>,
    failed: Option<Listing>,
    review: Option<Listing>,
}

/// Stateful dashboard watcher. A first successful read of a host establishes
/// that host's baseline; a failed read leaves it intact. The native shell
/// supplies loopback GETs and native notifications, while other callers can
/// supply their own adapters.
#[derive(Default)]
pub struct Watcher {
    memory: HashMap<String, HostMemory>,
    /// Hosts from the last `api/hosts` that parsed. A later failed read polls
    /// these again instead of assuming the fleet is only this machine.
    last_hosts: Vec<PollHost>,
}

impl Watcher {
    /// Poll the dashboard once. `get` reads a relative API route as JSON and
    /// returns `None` on failure; `notify` receives each announcement.
    pub fn poll(
        &mut self,
        mut get: impl FnMut(&str) -> Option<Value>,
        mut notify: impl FnMut(&str, &str),
    ) -> PollSummary {
        // `probe=false` lists the host file without opening an SSH session.
        // Reachability is the forward itself: probing every host on the tray's
        // 20-second poll would open those sessions in the background.
        let listed = get("api/hosts?probe=false").as_ref().and_then(listed_hosts);
        let (hosts, list_ok) = match listed {
            Some(hosts) => {
                let names: HashSet<&str> = hosts.iter().map(|host| host.name.as_str()).collect();
                self.memory.retain(|name, _| names.contains(name.as_str()));
                self.last_hosts.clone_from(&hosts);
                (hosts, true)
            }
            None => {
                if self.last_hosts.is_empty() {
                    (vec![local_fallback()], true)
                } else {
                    // The host list itself did not answer. Keep polling the
                    // hosts we already know, and do not report a partial sum
                    // that would count a missing host as zero.
                    (self.last_hosts.clone(), false)
                }
            }
        };
        let show_host = hosts.iter().any(|host| !host.local);

        let mut reads = Vec::with_capacity(hosts.len());
        for host in &hosts {
            if host.reachable == Some(false) {
                reads.push(HostRead {
                    name: host.name.clone(),
                    drains: None,
                    failed: None,
                    review: None,
                });
                continue;
            }
            let failed_route =
                format!("job-runs/all?state=failed&since={FAILED_SINCE}&limit={PAGE}");
            let drains = get(&api_path(host, "fleet/drains")).map(|body| running_drains(&body));
            let failed =
                get(&api_path(host, &failed_route)).map(|body| failed_runs(&body, &host.name));
            let review = review_snapshot(&mut get, host);
            reads.push(HostRead {
                name: host.name.clone(),
                drains,
                failed,
                review,
            });
        }

        let mut new_failed = Vec::new();
        let mut new_review = Vec::new();
        for read in &reads {
            let memory = self.memory.entry(read.name.clone()).or_default();
            if let Some(listing) = &read.failed {
                if let Some(seen) = &memory.failed_seen {
                    new_failed.extend(seen.added(listing).into_iter().cloned());
                }
                memory.failed_seen = Some(Seen::of(listing));
            }
            if let Some(listing) = &read.review {
                if let Some(seen) = &memory.review_seen {
                    new_review.extend(
                        listing
                            .items
                            .iter()
                            .filter(|item| !seen.keys.contains(&item.key))
                            .cloned(),
                    );
                }
                let next = match &memory.review_seen {
                    Some(seen) => seen.remember(listing),
                    None => Seen::of(listing),
                };
                memory.review_seen = Some(next);
            }
        }
        announce(
            &new_failed,
            "Run failed",
            "runs failed",
            show_host,
            &mut notify,
        );
        announce(
            &new_review,
            "Ready for review",
            "tasks ready for review",
            show_host,
            &mut notify,
        );

        let any = reads
            .iter()
            .any(|read| read.drains.is_some() || read.failed.is_some() || read.review.is_some());
        if !any {
            return PollSummary::Unreadable;
        }
        if !list_ok {
            return PollSummary::Live {
                drains: None,
                failed: None,
            };
        }
        PollSummary::Live {
            drains: sum_optional(reads.iter().map(|read| read.drains)),
            failed: sum_optional(
                reads
                    .iter()
                    .map(|read| read.failed.as_ref().and_then(|listing| listing.total)),
            ),
        }
    }
}

/// Sum every host that answered. One missing answer makes the total
/// unavailable: omitting that host would count it as zero.
fn sum_optional<T>(parts: impl Iterator<Item = Option<T>>) -> Option<T>
where
    T: std::ops::Add<Output = T> + Default,
{
    let mut total = T::default();
    let mut any = false;
    for part in parts {
        any = true;
        total = total + part?;
    }
    any.then_some(total)
}

fn local_fallback() -> PollHost {
    PollHost {
        name: String::new(),
        local: true,
        reachable: Some(true),
    }
}

fn listed_hosts(body: &Value) -> Option<Vec<PollHost>> {
    let rows = body.get("hosts")?.as_array()?;
    let mut hosts = Vec::new();
    let mut names = HashSet::new();
    for row in rows {
        let Some(name) = row.get("name").and_then(Value::as_str) else {
            continue;
        };
        if name.is_empty() || !names.insert(name.to_string()) {
            continue;
        }
        hosts.push(PollHost {
            name: name.to_string(),
            local: row.get("local").and_then(Value::as_bool).unwrap_or(false),
            reachable: match row.get("reachable") {
                Some(Value::Bool(value)) => Some(*value),
                _ => None,
            },
        });
    }
    if hosts.is_empty() { None } else { Some(hosts) }
}

fn api_path(host: &PollHost, rest: &str) -> String {
    if host.local {
        format!("api/{rest}")
    } else {
        format!("api/on/{}/{rest}", encode_segment(&host.name))
    }
}

/// Percent-encode one path segment. Host names are operator-chosen, and the
/// forward route is a single segment.
fn encode_segment(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for byte in name.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
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

fn failed_runs(body: &Value, host: &str) -> Listing {
    let items = rows(body)
        .filter_map(|run| {
            let task_title = run["tasks"]
                .as_array()
                .into_iter()
                .flatten()
                .find_map(|task| task["title"].as_str());
            let label = task_title.or_else(|| run["job_id"].as_str())?;
            Some(Item::new(
                host,
                run["workspace_id"].as_str()?,
                run["run_id"].as_str()?,
                label,
                &workspace_name(run),
            ))
        })
        .collect();
    Listing {
        items,
        total: body["total"].as_u64(),
        unavailable: unavailable_scopes(body, host),
    }
}

/// Workspaces a successful aggregate could not read. An absent `unavailable`
/// key means every source answered. The scope includes the host, so one
/// host's outage does not hide another host's workspace.
fn unavailable_scopes(body: &Value, host: &str) -> HashSet<(String, String)> {
    body["unavailable"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            entry["workspace_id"]
                .as_str()
                .map(|id| (host.to_string(), id.to_string()))
        })
        .collect()
}

/// Only a complete cursor walk may replace the review baseline. A later page
/// failure must not turn returning page-two tasks into new review transitions.
fn review_snapshot(
    get: &mut impl FnMut(&str) -> Option<Value>,
    host: &PollHost,
) -> Option<Listing> {
    let route = api_path(host, &format!("tasks/all?status=review&limit={PAGE}"));
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
        let page = review_tasks(&body, &host.name);
        listing.total = listing.total.or(page.total);
        listing.unavailable.extend(page.unavailable);
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

fn review_tasks(body: &Value, host: &str) -> Listing {
    let items = rows(body)
        .filter_map(|task| {
            Some(Item::new(
                host,
                task["workspace_id"].as_str()?,
                task["id"].as_str()?,
                task["title"].as_str()?,
                &workspace_name(task),
            ))
        })
        .collect();
    Listing {
        items,
        total: body["total"].as_u64(),
        unavailable: unavailable_scopes(body, host),
    }
}

impl Item {
    fn new(host: &str, workspace: &str, id: &str, label: &str, workspace_name: &str) -> Self {
        Self {
            key: ItemKey {
                host: host.to_string(),
                workspace: workspace.to_string(),
                id: id.to_string(),
            },
            label: label.to_string(),
            workspace: workspace_name.to_string(),
        }
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
/// The host is named only when the poll covers more than the serving host, so
/// a single-host fleet keeps the text the tray has always shown.
fn announce(
    added: &[Item],
    title: &str,
    plural: &str,
    show_host: bool,
    notify: &mut impl FnMut(&str, &str),
) {
    if added.len() > MAX_SEPARATE {
        let mut places: Vec<String> = added.iter().map(|item| place(item, show_host)).collect();
        places.sort_unstable();
        places.dedup();
        notify(
            &format!("{} {plural}", added.len()),
            &format!("In {}", places.join(", ")),
        );
        return;
    }
    for item in added {
        notify(title, &place_with_label(item, show_host));
    }
}

fn place(item: &Item, show_host: bool) -> String {
    if show_host {
        format!("{} · {}", item.workspace, item.key.host)
    } else {
        item.workspace.clone()
    }
}

fn place_with_label(item: &Item, show_host: bool) -> String {
    if show_host {
        format!("{} · {} · {}", item.label, item.workspace, item.key.host)
    } else {
        format!("{} · {}", item.label, item.workspace)
    }
}
