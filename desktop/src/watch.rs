//! Reads the app's own dashboard server for the tray summary and native
//! notifications.
//!
//! Every [`INTERVAL`] the watcher reads three `/api` routes of the server the
//! [`Sidecar`] runs: the fleet drain rows, the failed runs of the last day and
//! the tasks in review, all across this host's workspaces. The first answer
//! of each only records what is there; later ones notify for failed runs and
//! review tasks that were not in the previous answer.
//!
//! Notification text names what the dashboard's lists show for the item (a
//! task title, else the job name) and its workspace, never a run or task id.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tauri::{AppHandle, Url};

use crate::sidecar::Sidecar;
use crate::tray::{Summary, Tray};

/// Time between reads.
const INTERVAL: Duration = Duration::from_secs(20);
/// Upper bound on one request, connection included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// How far back failed runs are counted: the dashboard's summary window.
const FAILED_SINCE: &str = "24h";
/// Items read per list. The failed-run list is newest first, so new failures
/// always land in it; the review list is newest-created first.
const PAGE: usize = 200;
/// More new items than this in one read become a single notification.
const MAX_SEPARATE: usize = 3;

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

/// Read the dashboard server until the app exits.
pub(crate) fn run(app: AppHandle, sidecar: Arc<Sidecar>, tray: Tray) {
    init_notifications(&app);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        // The server is on loopback; a proxy from the environment must not
        // see these requests.
        .proxy(None)
        .build()
        .into();
    let mut failed_seen: Option<Seen> = None;
    let mut review_seen: Option<Seen> = None;
    while !sidecar.is_shutting_down() {
        let Some(base) = sidecar.dashboard() else {
            tray.show(&Summary::Offline);
            std::thread::sleep(INTERVAL);
            continue;
        };
        let drains = get(&agent, &base, "api/fleet/drains").map(|body| running_drains(&body));
        let failed = get(
            &agent,
            &base,
            &format!("api/job-runs/all?state=failed&since={FAILED_SINCE}&limit={PAGE}"),
        )
        .map(|body| failed_runs(&body));
        let review = get(
            &agent,
            &base,
            &format!("api/tasks/all?status=review&limit={PAGE}"),
        )
        .map(|body| review_tasks(&body));

        if drains.is_none() && failed.is_none() && review.is_none() {
            tray.show(&Summary::Unreadable);
        } else {
            tray.show(&Summary::Live {
                drains,
                failed: failed.as_ref().and_then(|listing| listing.total),
            });
        }
        if let Some(listing) = &failed {
            if let Some(seen) = &failed_seen {
                announce(&seen.added(listing), "Run failed", "runs failed");
            }
            failed_seen = Some(Seen::of(listing));
        }
        if let Some(listing) = &review {
            if let Some(seen) = &review_seen {
                announce(
                    &seen.added(listing),
                    "Ready for review",
                    "tasks ready for review",
                );
            }
            review_seen = Some(Seen::of(listing));
        }
        std::thread::sleep(INTERVAL);
    }
}

/// GET a dashboard route and parse its JSON body.
fn get(agent: &ureq::Agent, base: &Url, path: &str) -> Option<Value> {
    let url = base.join(path).ok()?;
    let body = agent
        .get(url.as_str())
        .call()
        .ok()?
        .body_mut()
        .read_to_string()
        .ok()?;
    serde_json::from_str(&body).ok()
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
fn announce(added: &[&Item], title: &str, plural: &str) {
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

/// Name the app notifications appear under. An unbundled development build
/// has no registered bundle, so on macOS it borrows Terminal's, as Tauri's
/// own notification plugin does.
fn init_notifications(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let identifier = if tauri::is_dev() {
            "com.apple.Terminal"
        } else {
            app.config().identifier.as_str()
        };
        let _ = notify_rust::set_application(identifier);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// Show a native notification. Failures are dropped: a notification is
/// advisory and the dashboard holds the same information.
pub(crate) fn notify(title: &str, body: &str) {
    let _ = notify_rust::Notification::new()
        .appname("Orbit")
        .summary(title)
        .body(body)
        .show();
}
