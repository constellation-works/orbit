//! Integration coverage at the polling/notification boundary used by the native shell.

#![allow(clippy::unwrap_used)]

use std::collections::{HashMap, HashSet};

use orbit_desktop::watcher::{PollSummary, Watcher};
use serde_json::{Value, json};
use url::Url;

struct Dashboard {
    /// Newest-created first, matching the task aggregate API.
    review: Vec<Value>,
    fail_page: Option<usize>,
    requests: Vec<(usize, usize)>,
    /// Every route the watcher asked for, in order.
    paths: Vec<String>,
    /// Review rows for these workspaces are omitted from this read.
    hidden_review: HashSet<String>,
    /// When set, `hidden_review` is also reported as `unavailable`.
    report_review_unavailable: bool,
    failed: Vec<Value>,
    /// Overrides `failed.len()` as the local failed-run total.
    failed_total: Option<u64>,
    drains: Vec<Value>,
    hidden_failed: HashSet<String>,
    report_failed_unavailable: bool,
    remote: Option<RemoteHost>,
}

struct RemoteHost {
    name: String,
    /// Every route for this host fails to answer.
    silent: bool,
    /// `false` lists the host as unreachable, so the watcher does not poll it.
    reachable: bool,
    drains: Vec<Value>,
    failed: Vec<Value>,
    failed_total: u64,
    review: Vec<Value>,
}

impl Dashboard {
    fn new(count: usize) -> Self {
        Self {
            review: (0..count).map(task).collect(),
            fail_page: None,
            requests: Vec::new(),
            hidden_review: HashSet::new(),
            report_review_unavailable: false,
            failed: Vec::new(),
            failed_total: None,
            drains: Vec::new(),
            hidden_failed: HashSet::new(),
            report_failed_unavailable: false,
            paths: Vec::new(),
            remote: None,
        }
    }

    fn get(&mut self, path: &str) -> Option<Value> {
        self.paths.push(path.to_string());
        let url = Url::parse(&format!("http://127.0.0.1/{path}")).unwrap();
        if let Some(rest) = url.path().strip_prefix("/api/on/") {
            return self.remote_route(rest, &url);
        }
        match url.path() {
            "/api/hosts" => return Some(self.hosts_body()),
            "/api/fleet/drains" => {
                return Some(json!({"items": self.drains, "total": self.drains.len()}));
            }
            "/api/job-runs/all" => return Some(self.failed_body()),
            "/api/tasks/all" => {}
            other => panic!("unexpected dashboard route: {other}"),
        }
        let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(query.get("status").unwrap(), "review");
        let limit = query.get("limit").unwrap().parse::<usize>().unwrap();
        assert!(limit > 0);
        let offset = query.get("cursor").map_or(0, |cursor| {
            cursor.strip_prefix("opaque +&/=").unwrap().parse().unwrap()
        });
        self.requests.push((offset, limit));
        if self.fail_page == Some(offset) {
            return None;
        }
        let visible: Vec<&Value> = self
            .review
            .iter()
            .filter(|task| match task["workspace_id"].as_str() {
                Some(id) => !self.hidden_review.contains(id),
                None => true,
            })
            .collect();
        let end = (offset + limit).min(visible.len());
        let mut body = json!({
            "items": &visible[offset..end],
            "total": visible.len(),
            "limit": limit,
            "offset": offset,
            "truncated": visible.len() > end - offset,
            "next_cursor": (end < visible.len()).then(|| format!("opaque +&/={end}")),
        });
        if self.report_review_unavailable {
            body["unavailable"] = unavailable_entries(&self.hidden_review);
        }
        Some(body)
    }

    fn hosts_body(&self) -> Value {
        let mut hosts = vec![json!({"name": "this-host", "local": true, "reachable": true})];
        if let Some(remote) = &self.remote {
            hosts.push(json!({
                "name": remote.name,
                "local": false,
                "reachable": remote.reachable,
            }));
        }
        json!({"hosts": hosts})
    }

    fn remote_route(&self, rest: &str, url: &Url) -> Option<Value> {
        let remote = self
            .remote
            .as_ref()
            .unwrap_or_else(|| panic!("remote route without a remote host: {rest}"));
        let (name, api) = rest
            .split_once('/')
            .unwrap_or_else(|| panic!("remote route has no host: {rest}"));
        assert_eq!(name, remote.name, "forward names the registered host");
        if remote.silent {
            return None;
        }
        match api {
            "fleet/drains" => Some(json!({"items": remote.drains, "total": remote.drains.len()})),
            "job-runs/all" => Some(json!({"items": remote.failed, "total": remote.failed_total})),
            "tasks/all" => {
                let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
                assert_eq!(query.get("status").unwrap().as_ref(), "review");
                Some(json!({
                    "items": remote.review,
                    "total": remote.review.len(),
                    "next_cursor": null,
                }))
            }
            other => panic!("unexpected remote route: {other}"),
        }
    }

    fn failed_body(&self) -> Value {
        let items: Vec<&Value> = self
            .failed
            .iter()
            .filter(|run| match run["workspace_id"].as_str() {
                Some(id) => !self.hidden_failed.contains(id),
                None => true,
            })
            .collect();
        let mut body = json!({
            "items": items,
            "total": self.failed_total.unwrap_or(items.len() as u64),
        });
        if self.report_failed_unavailable {
            body["unavailable"] = unavailable_entries(&self.hidden_failed);
        }
        body
    }

    fn poll(&mut self, watcher: &mut Watcher) -> Vec<(String, String)> {
        self.poll_summary(watcher).0
    }

    fn poll_summary(&mut self, watcher: &mut Watcher) -> (Vec<(String, String)>, PollSummary) {
        let mut notifications = Vec::new();
        let summary = watcher.poll(
            |path| self.get(path),
            |title, body| notifications.push((title.to_string(), body.to_string())),
        );
        (notifications, summary)
    }
}

fn unavailable_entries(workspaces: &HashSet<String>) -> Value {
    let mut ids: Vec<&String> = workspaces.iter().collect();
    ids.sort();
    json!(
        ids.iter()
            .map(|id| json!({
                "workspace_id": id,
                "workspace_name": "Workspace",
                "error": "workspace is unavailable",
            }))
            .collect::<Vec<_>>()
    )
}

fn task(index: usize) -> Value {
    task_in(index, "workspace-a", "Workspace A")
}

fn task_in(index: usize, workspace_id: &str, workspace_name: &str) -> Value {
    let seconds = 12 * 3600 - index;
    json!({
        "id": format!("task-{index}"),
        "title": format!("Task {index}"),
        "workspace_id": workspace_id,
        "workspace_name": workspace_name,
        "created_at": format!("2026-10-17T{:02}:{:02}:{:02}Z", seconds / 3600, seconds / 60 % 60, seconds % 60),
        "status": "review",
    })
}

fn failed_run(run_id: &str, workspace_id: &str, title: &str) -> Value {
    json!({
        "run_id": run_id,
        "workspace_id": workspace_id,
        "workspace_name": "Workspace A",
        "job_id": "job",
        "tasks": [{"title": title}],
    })
}

#[derive(Default)]
struct DrainDashboard {
    routes: HashMap<String, Value>,
    paths: Vec<String>,
}

impl DrainDashboard {
    fn drains(&mut self, prefix: &str, rows: Vec<Value>) {
        self.routes
            .insert(format!("{prefix}/fleet/drains"), json!({"items": rows}));
    }

    fn readiness(&mut self, prefix: &str, workspace: &str, tasks: Vec<Value>) {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("workspace", workspace)
            .append_pair("limit", "500")
            .finish();
        self.routes.insert(
            format!("{prefix}/workflows/auto/readiness?{query}"),
            json!({"total": tasks.len(), "tasks": tasks}),
        );
    }

    fn poll(&mut self, watcher: &mut Watcher) -> Vec<(String, String)> {
        let mut notes = Vec::new();
        watcher.poll(
            |path| {
                self.paths.push(path.into());
                self.routes.get(path).cloned()
            },
            |title, body| notes.push((title.into(), body.into())),
        );
        notes
    }
}

fn drain(workspace: &str, phase: &str, holds: &[&str]) -> Value {
    json!({
        "workspace_id": workspace,
        "workspace_name": "Workspace A",
        "phase": phase,
        "holds": holds,
    })
}

fn held_task(id: &str) -> Value {
    json!({"task_id": id, "reason": "baseline_red_hold"})
}

#[test]
fn a_running_drain_finishing_is_announced_once_after_a_quiet_first_read() {
    for phase in ["draining", "winding_down", "pulling"] {
        let mut dashboard = DrainDashboard::default();
        let mut watcher = Watcher::default();
        dashboard.drains(
            "api",
            vec![drain("workspace-a", phase, &["admissions stopped"])],
        );
        dashboard.readiness("api", "workspace-a", vec![held_task("private-old")]);
        assert!(dashboard.poll(&mut watcher).is_empty());
        dashboard.drains(
            "api",
            vec![drain("workspace-a", "idle", &["admissions stopped"])],
        );
        let notes = dashboard.poll(&mut watcher);
        assert_eq!(
            notes.len(),
            1,
            "{phase} followed by idle finishes one drain"
        );
        assert_eq!(notes[0].0, "Drain finished");
        assert!(notes[0].1.contains("Workspace A"));
        assert!(dashboard.poll(&mut watcher).is_empty());
    }
}

#[test]
fn new_holds_are_announced_once_and_task_identifiers_stay_private() {
    let mut dashboard = DrainDashboard::default();
    let mut watcher = Watcher::default();
    dashboard.drains("api", vec![drain("workspace-a", "draining", &[])]);
    dashboard.readiness("api", "workspace-a", vec![]);
    assert!(dashboard.poll(&mut watcher).is_empty());

    dashboard.drains(
        "api",
        vec![drain(
            "workspace-a",
            "draining",
            &["admissions stopped", "admissions stopped"],
        )],
    );
    dashboard.readiness(
        "api",
        "workspace-a",
        vec![held_task("private-new"), held_task("private-new")],
    );
    let notes = dashboard.poll(&mut watcher);
    assert_eq!(notes.len(), 2, "one fleet hold and one held task");
    assert!(notes.iter().any(|(title, _)| title == "Task held"));
    assert!(
        notes
            .iter()
            .any(|(_, body)| body.contains("admissions stopped"))
    );
    assert!(
        notes
            .iter()
            .all(|(_, body)| body.contains("Workspace A") && !body.contains("private-new"))
    );
    assert!(dashboard.poll(&mut watcher).is_empty());

    dashboard.readiness(
        "api",
        "workspace-a",
        vec![json!({"task_id": "private-new", "reason": "eligible"})],
    );
    assert!(dashboard.poll(&mut watcher).is_empty());
    dashboard.readiness("api", "workspace-a", vec![held_task("private-new")]);
    assert_eq!(
        dashboard.poll(&mut watcher).len(),
        1,
        "a hold that cleared can appear again"
    );
}

#[test]
fn unavailable_workspaces_do_not_finish_drains_or_reset_hold_history() {
    let mut dashboard = DrainDashboard::default();
    let mut watcher = Watcher::default();
    dashboard.drains("api", vec![drain("workspace-a", "unavailable", &[])]);
    assert!(dashboard.poll(&mut watcher).is_empty());
    dashboard.drains(
        "api",
        vec![drain("workspace-a", "idle", &["admissions stopped"])],
    );
    dashboard.readiness("api", "workspace-a", vec![held_task("private-old")]);
    assert!(
        dashboard.poll(&mut watcher).is_empty(),
        "first available workspace read establishes its baseline"
    );

    dashboard.drains(
        "api",
        vec![drain("workspace-a", "draining", &["admissions stopped"])],
    );
    assert!(dashboard.poll(&mut watcher).is_empty());
    dashboard.drains("api", vec![drain("workspace-a", "unavailable", &[])]);
    dashboard.paths.clear();
    assert!(
        dashboard.poll(&mut watcher).is_empty(),
        "unavailable is not idle"
    );
    assert!(
        dashboard
            .paths
            .iter()
            .all(|path| !path.contains("/readiness"))
    );
    dashboard.drains(
        "api",
        vec![drain("workspace-a", "draining", &["admissions stopped"])],
    );
    assert!(
        dashboard.poll(&mut watcher).is_empty(),
        "returning holds are already known"
    );

    dashboard.routes.remove("api/fleet/drains");
    assert!(
        dashboard.poll(&mut watcher).is_empty(),
        "a failed route is not a drain end"
    );
    dashboard.drains(
        "api",
        vec![drain("workspace-a", "idle", &["admissions stopped"])],
    );
    assert_eq!(dashboard.poll(&mut watcher)[0].0, "Drain finished");
}

#[test]
fn held_task_history_survives_failed_and_partial_readiness_reads() {
    let mut dashboard = DrainDashboard::default();
    let mut watcher = Watcher::default();
    dashboard.drains("api", vec![drain("workspace-a &/+", "idle", &[])]);
    dashboard.readiness("api", "workspace-a &/+", vec![held_task("private-old")]);
    assert!(dashboard.poll(&mut watcher).is_empty());
    let readiness_path = dashboard
        .routes
        .keys()
        .find(|path| path.contains("/readiness"))
        .unwrap()
        .clone();
    dashboard.routes.remove(&readiness_path);
    assert!(dashboard.poll(&mut watcher).is_empty());
    dashboard.routes.insert(
        readiness_path.clone(),
        json!({"total": 501, "tasks": [held_task("private-new")]}),
    );
    assert_eq!(dashboard.poll(&mut watcher).len(), 1);
    dashboard.readiness(
        "api",
        "workspace-a &/+",
        vec![held_task("private-old"), held_task("private-new")],
    );
    assert!(
        dashboard.poll(&mut watcher).is_empty(),
        "an omitted task in a partial snapshot did not leave its hold"
    );
    let path = Url::parse(&format!("http://127.0.0.1/{readiness_path}")).unwrap();
    assert!(
        path.query_pairs()
            .any(|(key, value)| key == "workspace" && value == "workspace-a &/+")
    );
}

#[test]
fn remote_drain_and_hold_transitions_are_scoped_by_host_and_capped() {
    let mut dashboard = DrainDashboard::default();
    dashboard.routes.insert(
        "api/hosts?probe=false".into(),
        json!({"hosts": [
            {"name": "local", "local": true}, {"name": "remote host", "local": false}
        ]}),
    );
    for prefix in ["api", "api/on/remote%20host"] {
        dashboard.drains(prefix, vec![drain("workspace-a", "draining", &[])]);
        dashboard.readiness(prefix, "workspace-a", vec![held_task("private-old")]);
    }
    let mut watcher = Watcher::default();
    assert!(dashboard.poll(&mut watcher).is_empty());
    dashboard.routes.remove("api/on/remote%20host/fleet/drains");
    dashboard.drains("api", vec![drain("workspace-a", "idle", &[])]);
    let notes = dashboard.poll(&mut watcher);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].0, "Drain finished");
    assert!(notes[0].1.ends_with("local"));

    dashboard.drains(
        "api/on/remote%20host",
        vec![drain("workspace-a", "idle", &[])],
    );
    dashboard.readiness(
        "api/on/remote%20host",
        "workspace-a",
        (0..4).map(|i| held_task(&format!("private-{i}"))).collect(),
    );
    let notes = dashboard.poll(&mut watcher);
    assert_eq!(
        notes.len(),
        2,
        "one remote drain end and one summary for four holds"
    );
    assert!(notes.iter().any(|(title, _)| title == "Drain finished"));
    assert!(notes.iter().any(|(title, _)| title == "4 tasks held"));
    assert!(
        notes
            .iter()
            .all(|(_, body)| body.contains("remote host") && !body.contains("private-"))
    );
    assert!(dashboard.poll(&mut watcher).is_empty());
}

#[test]
fn an_older_task_entering_review_beyond_two_full_pages_is_announced() {
    let mut dashboard = Dashboard::new(400);
    let mut watcher = Watcher::default();
    assert!(dashboard.poll(&mut watcher).is_empty());
    dashboard.review.push(task(400));
    dashboard.requests.clear();

    assert_eq!(
        dashboard.poll(&mut watcher),
        vec![("Ready for review".into(), "Task 400 · Workspace A".into())]
    );
    let limit = dashboard.requests[0].1;
    assert_eq!(
        dashboard.requests.len(),
        dashboard.review.len().div_ceil(limit)
    );
    for (page, (offset, size)) in dashboard.requests.iter().enumerate() {
        assert_eq!(
            *size, limit,
            "the cursor remains bound to the same page size"
        );
        assert_eq!(*offset, page * limit);
    }
    assert!(dashboard.poll(&mut watcher).is_empty());
}

#[test]
fn an_unchanged_task_moving_onto_the_first_page_is_not_announced() {
    let mut dashboard = Dashboard::new(201);
    let mut watcher = Watcher::default();
    assert!(dashboard.poll(&mut watcher).is_empty());
    dashboard.review.remove(0);

    assert!(dashboard.poll(&mut watcher).is_empty());
}

#[test]
fn a_failed_later_page_preserves_the_complete_baseline_until_recovery() {
    let mut dashboard = Dashboard::new(201);
    let mut watcher = Watcher::default();
    assert!(dashboard.poll(&mut watcher).is_empty());
    dashboard.review.push(task(201));
    dashboard.fail_page = Some(200);

    assert!(dashboard.poll(&mut watcher).is_empty());
    dashboard.fail_page = None;
    assert_eq!(
        dashboard.poll(&mut watcher),
        vec![("Ready for review".into(), "Task 201 · Workspace A".into())]
    );
    assert!(dashboard.poll(&mut watcher).is_empty());
}

#[test]
fn an_incomplete_initial_read_does_not_establish_a_baseline() {
    let mut dashboard = Dashboard::new(201);
    let mut watcher = Watcher::default();
    dashboard.fail_page = Some(200);
    assert!(dashboard.poll(&mut watcher).is_empty());

    dashboard.fail_page = None;
    assert!(dashboard.poll(&mut watcher).is_empty());
    dashboard.review.push(task(201));
    assert_eq!(
        dashboard.poll(&mut watcher),
        vec![("Ready for review".into(), "Task 201 · Workspace A".into())]
    );
}

#[test]
fn a_review_task_is_not_reannounced_when_its_workspace_recovers() {
    let mut dashboard = Dashboard::new(0);
    for index in 0..4 {
        dashboard.review.push(task(index));
    }
    dashboard
        .review
        .push(task_in(100, "workspace-b", "Workspace B"));
    let mut watcher = Watcher::default();
    assert!(dashboard.poll(&mut watcher).is_empty());

    dashboard.hidden_review.insert("workspace-a".into());
    dashboard.report_review_unavailable = true;
    assert!(dashboard.poll(&mut watcher).is_empty());
    assert!(
        dashboard.poll(&mut watcher).is_empty(),
        "a workspace that stays unavailable keeps the remembered review tasks"
    );

    dashboard.hidden_review.clear();
    dashboard.report_review_unavailable = false;
    assert!(
        dashboard.poll(&mut watcher).is_empty(),
        "tasks that stayed in review must not be announced when their workspace recovers"
    );
}

#[test]
fn a_new_review_task_after_workspace_recovery_is_announced_once() {
    let mut dashboard = Dashboard::new(1);
    let mut watcher = Watcher::default();
    assert!(dashboard.poll(&mut watcher).is_empty());

    dashboard.hidden_review.insert("workspace-a".into());
    dashboard.report_review_unavailable = true;
    assert!(dashboard.poll(&mut watcher).is_empty());

    dashboard.hidden_review.clear();
    dashboard.report_review_unavailable = false;
    dashboard.review.push(task(1));
    assert_eq!(
        dashboard.poll(&mut watcher),
        vec![("Ready for review".into(), "Task 1 · Workspace A".into())]
    );
    assert!(dashboard.poll(&mut watcher).is_empty());
}

#[test]
fn a_review_task_omitted_without_unavailable_is_reannounced() {
    let mut dashboard = Dashboard::new(1);
    dashboard
        .review
        .push(task_in(1, "workspace-b", "Workspace B"));
    let mut watcher = Watcher::default();
    assert!(dashboard.poll(&mut watcher).is_empty());

    dashboard.hidden_review.insert("workspace-a".into());
    assert!(dashboard.poll(&mut watcher).is_empty());

    dashboard.hidden_review.clear();
    assert_eq!(
        dashboard.poll(&mut watcher),
        vec![("Ready for review".into(), "Task 0 · Workspace A".into())]
    );
}

#[test]
fn a_failed_run_from_an_unavailable_workspace_is_not_announced_again() {
    let mut dashboard = Dashboard::new(0);
    dashboard
        .failed
        .push(failed_run("run-old", "workspace-a", "Old failure"));
    let mut watcher = Watcher::default();
    assert!(dashboard.poll(&mut watcher).is_empty());

    dashboard.hidden_failed.insert("workspace-a".into());
    dashboard.report_failed_unavailable = true;
    assert!(dashboard.poll(&mut watcher).is_empty());

    dashboard.hidden_failed.clear();
    dashboard.report_failed_unavailable = false;
    dashboard
        .failed
        .push(failed_run("run-new", "workspace-a", "New failure"));
    assert!(
        dashboard.poll(&mut watcher).is_empty(),
        "failed-run recovery still suppresses every item of a workspace that was unavailable, including a failure first seen after recovery"
    );
}

#[test]
fn a_remote_failed_run_names_the_host_even_when_the_run_id_matches_locally() {
    let mut dashboard = Dashboard::new(0);
    dashboard
        .failed
        .push(failed_run("run-1", "workspace-a", "Local failure"));
    dashboard.remote = Some(RemoteHost {
        name: "owner".into(),
        silent: false,
        reachable: true,
        drains: Vec::new(),
        failed: Vec::new(),
        failed_total: 0,
        review: Vec::new(),
    });
    let mut watcher = Watcher::default();
    assert!(dashboard.poll(&mut watcher).is_empty());
    dashboard.paths.clear();

    let remote = dashboard.remote.as_mut().unwrap();
    remote
        .failed
        .push(failed_run("run-1", "workspace-a", "Remote failure"));
    remote.failed_total = 1;
    assert_eq!(
        dashboard.poll(&mut watcher),
        vec![(
            "Run failed".into(),
            "Remote failure · Workspace A · owner".into()
        )]
    );
    let host_lists: Vec<&str> = dashboard
        .paths
        .iter()
        .filter(|path| path.starts_with("api/hosts"))
        .map(String::as_str)
        .collect();
    assert_eq!(
        host_lists,
        vec!["api/hosts?probe=false"],
        "each poll lists hosts once and does not probe them"
    );
    assert!(
        dashboard
            .paths
            .iter()
            .any(|path| path.starts_with("api/on/owner/job-runs/all?")),
        "the remote failed-run route is read through the host forward"
    );
    assert!(dashboard.poll(&mut watcher).is_empty());
}

#[test]
fn tray_counts_sum_across_hosts_and_a_silent_host_keeps_its_baseline() {
    let mut dashboard = Dashboard::new(0);
    dashboard.drains = vec![json!({"phase": "draining"}), json!({"phase": "idle"})];
    dashboard
        .failed
        .push(failed_run("local-old", "workspace-a", "Local old"));
    dashboard.failed_total = Some(2);
    dashboard.remote = Some(RemoteHost {
        name: "owner".into(),
        silent: false,
        reachable: true,
        drains: vec![
            json!({"phase": "pulling"}),
            json!({"phase": "winding_down"}),
            json!({"phase": "unavailable"}),
        ],
        failed: vec![failed_run("remote-old", "workspace-b", "Remote old")],
        failed_total: 3,
        review: vec![task_in(7, "workspace-b", "Workspace B")],
    });
    let mut watcher = Watcher::default();

    let (notes, summary) = dashboard.poll_summary(&mut watcher);
    assert!(notes.is_empty(), "the first read only records the baseline");
    assert_eq!(
        summary,
        PollSummary::Live {
            drains: Some(3),
            failed: Some(5)
        }
    );

    dashboard.remote.as_mut().unwrap().silent = true;
    dashboard
        .failed
        .push(failed_run("local-new", "workspace-a", "Local new"));
    dashboard.failed_total = Some(3);
    let (notes, summary) = dashboard.poll_summary(&mut watcher);
    assert_eq!(
        notes,
        vec![(
            "Run failed".into(),
            "Local new · Workspace A · this-host".into()
        )]
    );
    assert_eq!(
        summary,
        PollSummary::Live {
            drains: None,
            failed: None
        },
        "a host that returns nothing is unavailable, not a zero counted into the sum"
    );

    dashboard.remote.as_mut().unwrap().silent = false;
    let (notes, summary) = dashboard.poll_summary(&mut watcher);
    assert!(
        notes.is_empty(),
        "failed runs and review tasks the silent host already had are not announced when it returns"
    );
    assert_eq!(
        summary,
        PollSummary::Live {
            drains: Some(3),
            failed: Some(6)
        }
    );

    let remote = dashboard.remote.as_mut().unwrap();
    remote.reachable = false;
    dashboard.paths.clear();
    let (notes, summary) = dashboard.poll_summary(&mut watcher);
    assert!(
        notes.is_empty(),
        "a host the list marks unreachable keeps its baseline"
    );
    assert_eq!(
        summary,
        PollSummary::Live {
            drains: None,
            failed: None
        },
        "an unreachable host is unavailable, not zero"
    );
    assert!(
        dashboard
            .paths
            .iter()
            .all(|path| !path.starts_with("api/on/")),
        "an unreachable host is not polled"
    );
}
