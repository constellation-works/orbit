//! Integration coverage at the polling/notification boundary used by the native shell.

#![allow(clippy::unwrap_used)]

use orbit_desktop::watcher::Watcher;
use serde_json::{Value, json};
use url::Url;

struct Dashboard {
    /// Newest-created first, matching the task aggregate API.
    review: Vec<Value>,
    fail_page: Option<usize>,
    requests: Vec<(usize, usize)>,
}

impl Dashboard {
    fn new(count: usize) -> Self {
        Self {
            review: (0..count).map(task).collect(),
            fail_page: None,
            requests: Vec::new(),
        }
    }

    fn get(&mut self, path: &str) -> Option<Value> {
        let url = Url::parse(&format!("http://127.0.0.1/{path}")).unwrap();
        match url.path() {
            "/api/fleet/drains" | "/api/job-runs/all" => {
                return Some(json!({"items": [], "total": 0}));
            }
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
        let end = (offset + limit).min(self.review.len());
        Some(json!({
            "items": self.review[offset..end],
            "total": self.review.len(),
            "limit": limit,
            "offset": offset,
            "truncated": self.review.len() > end - offset,
            "next_cursor": (end < self.review.len()).then(|| format!("opaque +&/={end}")),
        }))
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

fn task(index: usize) -> Value {
    let seconds = 12 * 3600 - index;
    json!({
        "id": format!("task-{index}"),
        "title": format!("Task {index}"),
        "workspace_id": "workspace-a",
        "workspace_name": "Workspace A",
        "created_at": format!("2026-10-17T{:02}:{:02}:{:02}Z", seconds / 3600, seconds / 60 % 60, seconds % 60),
        "status": "review",
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
