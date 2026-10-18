//! Integration coverage at the polling/notification boundary used by the native shell.

#![allow(clippy::unwrap_used)]

use std::collections::HashSet;

use orbit_desktop::watcher::Watcher;
use serde_json::{Value, json};
use url::Url;

struct Dashboard {
    /// Newest-created first, matching the task aggregate API.
    review: Vec<Value>,
    fail_page: Option<usize>,
    requests: Vec<(usize, usize)>,
    /// Review rows for these workspaces are omitted from this read.
    hidden_review: HashSet<String>,
    /// When set, `hidden_review` is also reported as `unavailable`.
    report_review_unavailable: bool,
    failed: Vec<Value>,
    hidden_failed: HashSet<String>,
    report_failed_unavailable: bool,
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
            hidden_failed: HashSet::new(),
            report_failed_unavailable: false,
        }
    }

    fn get(&mut self, path: &str) -> Option<Value> {
        let url = Url::parse(&format!("http://127.0.0.1/{path}")).unwrap();
        match url.path() {
            "/api/fleet/drains" => return Some(json!({"items": [], "total": 0})),
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
            "total": items.len(),
        });
        if self.report_failed_unavailable {
            body["unavailable"] = unavailable_entries(&self.hidden_failed);
        }
        body
    }

    fn poll(&mut self, watcher: &mut Watcher) -> Vec<(String, String)> {
        let mut notifications = Vec::new();
        watcher.poll(
            |path| self.get(path),
            |title, body| notifications.push((title.to_string(), body.to_string())),
        );
        notifications
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
