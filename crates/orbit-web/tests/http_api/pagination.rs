use std::collections::BTreeSet;
use std::fs;

use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, TaskStatus};
use orbit_types::task::{Task, TaskType};
use orbit_types::workspace::WorkspaceCheckout;
use serde_json::{Value, json};

use super::support::{Fixture, Server, isolated, json_ok, sent};

const FILTER: &str = "status=backlog,proposed&tags=page,release&type=bug&q=needle&limit=3";
const WORKSPACE: &str = "ws_http_fixture";
const OTHER: &str = "ws_http_pagination";

fn add_workspace(fixture: &Fixture) -> OrbitRuntime {
    let path = orbit_registry::workspace_registry::registry_path_for(&fixture.global);
    let mut registry = orbit_registry::workspace_registry::load_registry_from(&path).unwrap();
    let mut workspace = registry.workspaces[0].clone();
    workspace.id = OTHER.into();
    workspace.name = "pagination".into();
    let repo = fixture.path("pagination");
    let work = repo.join(".orbit");
    fs::create_dir_all(&work).unwrap();
    fs::write(
        work.join("config.yaml"),
        format!("schema_version: 1\nworkspace_id: {OTHER}\n"),
    )
    .unwrap();
    let checkout = WorkspaceCheckout::owner(OTHER.into(), repo, work);
    registry.workspaces.push(workspace.clone());
    registry.checkouts.push(checkout.clone());
    orbit_registry::workspace_registry::save_registry_to(&registry, &path).unwrap();
    orbit_cmd::registry_runtime::RegisteredRuntimeFactory::open_registered_checkout(
        &fixture.global,
        &workspace,
        &checkout,
    )
    .unwrap()
}

fn matching_task() -> TaskAddParams {
    TaskAddParams {
        title: "Needle pagination match".into(),
        description: "HTTP pagination fixture".into(),
        tags: vec!["page".into(), "release".into()],
        task_type: Some(TaskType::Bug),
        status: Some(TaskStatus::Backlog),
        ..Default::default()
    }
}

fn seed(fixture: &Fixture, other: &OrbitRuntime) -> Vec<(String, Task)> {
    let mut matches = Vec::new();
    // Interleave workspaces so an aggregate must merge, not concatenate pages.
    for index in 0..7 {
        for (workspace, runtime) in [(WORKSPACE, &fixture.runtime), (OTHER, other)] {
            let mut params = matching_task();
            if index % 2 == 0 {
                params.status = Some(TaskStatus::Proposed);
                params.title = format!("Pagination NEEDLE match {index}");
            }
            matches.push((workspace.into(), runtime.add_task(params).unwrap()));
        }
    }
    // Newer rows matching all but one predicate must not consume the page limit.
    for runtime in [&fixture.runtime, other] {
        for mismatch in 0..5 {
            let mut params = matching_task();
            match mismatch {
                0 => params.status = Some(TaskStatus::Done),
                1 => params.tags = vec!["page".into()],
                2 => params.tags = vec!["release".into()],
                3 => params.task_type = Some(TaskType::Chore),
                _ => params.title = "Unrelated search text".into(),
            }
            runtime.add_task(params).unwrap();
        }
    }
    matches.sort_by(|(_, a), (_, b)| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    matches
}

fn page(server: &Server, endpoint: &str, filter: &str, cursor: Option<&str>) -> Value {
    let mut request = server.request("GET", &format!("{endpoint}?{filter}"));
    if let Some(cursor) = cursor {
        request = request.query(&[("cursor", cursor)]);
    }
    json_ok(sent("GET", endpoint, request))
}

fn ids(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn filtered_pages_reach_every_match_and_continue_stably_on_both_endpoints() {
    isolated(
        "pagination::filtered_pages_reach_every_match_and_continue_stably_on_both_endpoints",
        || {
            for aggregate in [false, true] {
                let fixture = Fixture::new();
                let other = add_workspace(&fixture);
                let matches = seed(&fixture, &other);
                let expected: Vec<_> = matches
                    .iter()
                    .filter(|(workspace, _)| aggregate || workspace == WORKSPACE)
                    .map(|(_, task)| task.id.clone())
                    .collect();
                let endpoint = if aggregate {
                    "/api/tasks/all"
                } else {
                    "/api/tasks"
                };
                let filter = format!("{FILTER}&workspace={WORKSPACE}");
                let server = fixture.server(true);
                let mut current = page(&server, endpoint, &filter, None);
                assert_eq!(current["total"], expected.len());
                assert_eq!(current, page(&server, endpoint, &filter, None));

                // A newer matching insert must not shift a keyset continuation.
                let inserted = json_ok(server.send(
                    "POST",
                    &format!("/api/tasks?workspace={WORKSPACE}"),
                    json!({"title":"Needle newer match", "description":"HTTP insert",
                        "tags":["page","release"], "task_type":"bug", "status":"backlog",
                        "complexity":"low"}),
                ));
                let inserted_id = inserted["id"].as_str().unwrap();
                let mut seen = Vec::new();
                let mut pages = 0;
                loop {
                    let rows = current["items"].as_array().unwrap();
                    assert_eq!(current["offset"], seen.len());
                    assert_eq!(current["limit"], 3);
                    assert_eq!(rows.len(), 3.min(expected.len() - seen.len()));
                    assert_eq!(current["total"], expected.len() + usize::from(pages > 0));
                    if aggregate {
                        for row in rows {
                            let owner = &matches
                                .iter()
                                .find(|(_, task)| task.id == row["id"])
                                .unwrap()
                                .0;
                            assert_eq!(row["workspace_id"], *owner);
                        }
                    }
                    seen.extend(ids(&current));
                    pages += 1;
                    assert!(pages <= expected.len(), "continuation must terminate");
                    let Some(cursor) = current["next_cursor"].as_str() else {
                        break;
                    };
                    let next = page(&server, endpoint, &filter, Some(cursor));
                    assert_eq!(
                        next,
                        page(&server, endpoint, &filter, Some(cursor)),
                        "replaying a continuation is stable"
                    );
                    current = next;
                }
                assert!(pages > 1);
                assert_eq!(
                    seen, expected,
                    "all filtered matches in order, with no omissions"
                );
                assert_eq!(
                    seen.iter().collect::<BTreeSet<_>>().len(),
                    seen.len(),
                    "no duplicates"
                );
                let restarted = page(&server, endpoint, &filter, None);
                assert_eq!(restarted["total"], expected.len() + 1);
                assert_eq!(
                    ids(&restarted)[0],
                    inserted_id,
                    "a fresh traversal sees the insert"
                );
            }
        },
    );
}

fn refused(server: &Server, endpoint: &str, filter: &str, cursor: &str) {
    let response = server
        .request("GET", &format!("{endpoint}?{filter}"))
        .query(&[("cursor", cursor)])
        .send()
        .unwrap();
    let status = response.status().as_u16();
    let body: Value = response.json().unwrap();
    assert_eq!(status, 400, "cursor refusal on {endpoint}: {body}");
    assert!(body["error"].is_string(), "structured refusal: {body}");
}

fn snapshot(runtime: &OrbitRuntime) -> Value {
    json!(
        runtime
            .list_tasks()
            .unwrap()
            .iter()
            .map(|task| json!({
                "task": task,
                "comments": runtime.get_task_comments(&task.id).unwrap(),
                "history": runtime.get_task_history(&task.id).unwrap(),
                "artifacts": runtime.get_task_artifacts(&task.id).unwrap(),
            }))
            .collect::<Vec<_>>()
    )
}

#[test]
fn invalid_filter_workspace_and_endpoint_cursors_are_refused_without_state_changes() {
    isolated(
        "pagination::invalid_filter_workspace_and_endpoint_cursors_are_refused_without_state_changes",
        || {
            let fixture = Fixture::new();
            let other = add_workspace(&fixture);
            seed(&fixture, &other);
            let server = fixture.server(false);
            let filter = format!("{FILTER}&workspace={WORKSPACE}");
            let before = [snapshot(&fixture.runtime), snapshot(&other)];
            let endpoints = ["/api/tasks", "/api/tasks/all"];
            let first = endpoints.map(|endpoint| page(&server, endpoint, &filter, None));
            for (index, endpoint) in endpoints.iter().enumerate() {
                let cursor = first[index]["next_cursor"].as_str().unwrap();
                for malformed in ["not!a-cursor".to_string(), "e30".into(), "x".repeat(4097)] {
                    refused(&server, endpoint, &filter, &malformed);
                }
                for changed in [
                    FILTER.replace("backlog,proposed", "backlog"),
                    FILTER.replace("page,release", "page"),
                    FILTER.replace("type=bug", "type=chore"),
                    FILTER.replace("q=needle", "q=other"),
                    FILTER.replace("limit=3", "limit=2"),
                ] {
                    refused(
                        &server,
                        endpoint,
                        &format!("{changed}&workspace={WORKSPACE}"),
                        cursor,
                    );
                }
                refused(&server, endpoints[1 - index], &filter, cursor);
                if *endpoint == "/api/tasks" {
                    refused(
                        &server,
                        endpoint,
                        &format!("{FILTER}&workspace={OTHER}"),
                        cursor,
                    );
                }
                assert_eq!(page(&server, endpoint, &filter, None), first[index]);
                let continuation = page(&server, endpoint, &filter, Some(cursor));
                assert_eq!(
                    continuation["offset"], 3,
                    "refusals preserve the valid continuation"
                );
                assert_eq!(continuation["items"].as_array().unwrap().len(), 3);
                assert_eq!(
                    [snapshot(&fixture.runtime), snapshot(&other)],
                    before,
                    "refused cursors preserve tasks, history, comments, and artifacts in both workspaces"
                );
            }
            // Aggregate scope is the active workspace set, not the workspace query parameter.
            let single = Fixture::new();
            let single_server = single.server(false);
            refused(
                &single_server,
                "/api/tasks/all",
                &filter,
                first[1]["next_cursor"].as_str().unwrap(),
            );
            assert!(single.runtime.list_tasks().unwrap().is_empty());
        },
    );
}
