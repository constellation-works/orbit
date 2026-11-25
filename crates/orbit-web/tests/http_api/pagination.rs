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
    // Interleave creation across workspaces so the aggregate must regroup
    // rows by workspace, and a page boundary falls inside a workspace.
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
                let endpoint = if aggregate {
                    "/api/tasks/all"
                } else {
                    "/api/tasks"
                };
                let filter = format!("{FILTER}&workspace={WORKSPACE}");
                let server = fixture.server(true);
                // The aggregate reads workspace by workspace, in the order the
                // workspace picker lists them, so a status section never shows
                // one workspace's rows on both sides of another's across pages.
                let picker_order: Vec<String> = json_ok(server.get("/api/workspaces"))
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|workspace| workspace["id"].as_str().unwrap().to_string())
                    .collect();
                let mut expected: Vec<_> = matches
                    .iter()
                    .filter(|(workspace, _)| aggregate || workspace == WORKSPACE)
                    .collect();
                expected.sort_by_key(|(workspace, _)| {
                    picker_order.iter().position(|id| id == workspace).unwrap()
                });
                let expected: Vec<_> = expected
                    .into_iter()
                    .map(|(_, task)| task.id.clone())
                    .collect();
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

/// The dashboard Tasks board's default grouped read.
const BOARD: &str = "group=status&status=in-progress,review,blocked,proposed,backlog&limit=2&status_limit=backlog:3";

fn freshness_scans(server: &Server, read: impl FnOnce() -> Value) -> (Value, usize) {
    let before = server.task_query_trace().len();
    let body = read();
    let scans = server.task_query_trace()[before..]
        .iter()
        .filter(|row| row["span"]["name"] == "task_index_freshness")
        .count();
    (body, scans)
}

fn section<'a>(grouped: &'a Value, status: &str) -> &'a Value {
    grouped["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|section| section["status"] == status)
        .unwrap_or_else(|| panic!("no {status} section: {grouped}"))
}

/// A settled grouped request validates the workspace task index once for all
/// its sections, and each section answers exactly what the ungrouped listing
/// of that status answers. A later request still sees status moves and an
/// out-of-band envelope edit, which the stale index's fallback serves.
#[test]
fn grouped_task_sections_share_one_index_freshness_validation() {
    isolated(
        "pagination::grouped_task_sections_share_one_index_freshness_validation",
        || {
            let fixture = Fixture::new();
            let runtime = &fixture.runtime;
            let add = |title: &str, status| {
                let mut params = matching_task();
                params.title = title.into();
                params.status = Some(status);
                runtime.add_task(params).unwrap()
            };
            let mut backlog = Vec::new();
            for index in 0..5 {
                backlog.push(add(&format!("Needle backlog {index}"), TaskStatus::Backlog));
            }
            let mover = add("Needle mover", TaskStatus::Proposed);
            add("Needle proposed", TaskStatus::Proposed);
            add("Needle active", TaskStatus::InProgress);
            add("Needle review", TaskStatus::Review);
            add("Elsewhere", TaskStatus::Backlog);
            let detail = &backlog[4].id;
            let server = fixture.counted_task_server();
            let board = format!("{BOARD}&q=needle&tags=page&detail={detail}");
            let read = || {
                json_ok(sent(
                    "GET",
                    "/api/tasks",
                    server.request("GET", &format!("/api/tasks?{board}")),
                ))
            };
            // Settle the index (and any envelope proofs) before counting.
            read();
            let (grouped, scans) = freshness_scans(&server, read);
            assert_eq!(
                scans, 1,
                "one whole-workspace freshness validation serves every section"
            );
            let order: Vec<_> = grouped["sections"]
                .as_array()
                .unwrap()
                .iter()
                .map(|section| section["status"].as_str().unwrap())
                .collect();
            assert_eq!(
                order,
                ["in-progress", "review", "blocked", "proposed", "backlog"]
            );
            assert_eq!(grouped["total"], 9, "{grouped}");
            assert_eq!(grouped["detail"]["id"], json!(detail), "{grouped}");
            for (status, limit, total) in [
                ("in-progress", 2, 1),
                ("review", 2, 1),
                ("blocked", 2, 0),
                ("proposed", 2, 2),
                ("backlog", 3, 5),
            ] {
                let section = section(&grouped, status);
                let flat = page(
                    &server,
                    "/api/tasks",
                    &format!("status={status}&q=needle&tags=page&limit={limit}"),
                    None,
                );
                assert_eq!(section["total"], total, "{status}: {section}");
                assert_eq!(section["limit"], limit, "{status}: {section}");
                for key in ["items", "total", "truncated", "offset"] {
                    assert_eq!(section[key], flat[key], "{status} {key} parity");
                }
                assert_eq!(
                    section["next_cursor"].is_string(),
                    total > limit,
                    "{status}: {section}"
                );
            }
            // A section's cursor still continues that status alone.
            let rest = page(
                &server,
                "/api/tasks",
                "group=status&status=backlog&q=needle&tags=page&limit=3",
                section(&grouped, "backlog")["next_cursor"].as_str(),
            );
            let mut reached = ids(section(&grouped, "backlog"));
            reached.extend(ids(&rest["sections"][0]));
            let mut expected: Vec<_> = backlog.iter().map(|task| task.id.clone()).collect();
            expected.reverse();
            assert_eq!(reached, expected);
            // Errors are unchanged: a cursor still binds one section.
            let refused = sent(
                "GET",
                "/api/tasks",
                server.request(
                    "GET",
                    "/api/tasks?group=status&status=backlog,proposed&cursor=x",
                ),
            );
            assert_eq!(refused.status(), 400);

            // A status move between requests shows on the next request.
            runtime
                .update_task_as_human(
                    &mover.id,
                    orbit_core::application::task::TaskUpdateParams {
                        status: Some(TaskStatus::Backlog),
                        ..Default::default()
                    },
                    "test operator".into(),
                )
                .unwrap();
            let moved = read();
            assert_eq!(section(&moved, "proposed")["total"], 1, "{moved}");
            assert_eq!(section(&moved, "backlog")["total"], 6, "{moved}");
            assert_eq!(
                section(&moved, "backlog")["items"][0]["id"],
                json!(mover.id)
            );

            // An out-of-band envelope edit leaves the index stale. The next
            // grouped request takes the unbatched fallback and answers from
            // the envelope: the retagged task no longer matches the tag filter.
            let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
                &orbit_store::maintenance::task_registry::task_registry_path(&fixture.global),
            )
            .unwrap();
            let binding = registry.find_task_binding(&mover.id).unwrap().unwrap();
            let envelope_path = binding.canonical_path.join("task.yaml");
            let mut envelope: orbit_types::task::TaskEnvelopeV2 =
                serde_yaml::from_str(&fs::read_to_string(&envelope_path).unwrap()).unwrap();
            envelope.tags = vec!["release".into()];
            fs::write(&envelope_path, serde_yaml::to_string(&envelope).unwrap()).unwrap();
            let (edited, scans) = freshness_scans(&server, read);
            assert!(
                scans > 1,
                "a stale index is not trusted for the batch: {scans}"
            );
            assert_eq!(section(&edited, "backlog")["total"], 5, "{edited}");
            assert!(
                !ids(section(&edited, "backlog")).contains(&mover.id),
                "{edited}"
            );
            let (_, scans) = freshness_scans(&server, read);
            assert_eq!(
                scans, 1,
                "the fallback repaired the index for later requests"
            );
        },
    );
}
