use std::str::FromStr;

use orbit_search::bm25_top_k;

use super::*;

#[test]
fn global_search_single_kind_limit_keeps_task_behavior() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let query = "taskonly";
    seed_search_fixture(&runtime, query, 20);

    let response = runtime
        .global_search(GlobalSearchParams {
            query: Some(query.to_string()),
            kind: GlobalSearchKind::Task,
            limit: 8,
            ..Default::default()
        })
        .expect("search tasks");

    assert_eq!(response.results.len(), 8);
    assert!(response.results.iter().all(|hit| hit.kind == "task"));
}
#[test]
fn task_create_synchronously_indexes_non_adjacent_title_terms() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let id = add_task(
        &runtime,
        "indexed lexical route proves FTS",
        "ordinary body",
        TaskStatus::Backlog,
    );
    // The bundle fallback treats this as one contiguous substring and cannot
    // match it. A hit therefore proves the lexical branch consulted FTS5,
    // whose query syntax requires both terms without requiring adjacency.
    let response = runtime
        .global_search(GlobalSearchParams {
            query: Some("indexed proves".to_string()),
            kind: GlobalSearchKind::Task,
            ..Default::default()
        })
        .expect("FTS lexical task search");

    assert_eq!(response.results.len(), 1);
    assert_eq!(response.results[0].source, "lexical");
    assert_eq!(response.results[0].id.as_deref(), Some(id.as_str()));
}

/// [DANI-10445] The index carries only title, description, plan, execution
/// summary, and acceptance criteria. Once one task chunk exists, a query that
/// matches only a comment, an `external_refs` id, or an artifact manifest
/// path must still find the task through the bundle matcher, exactly as it
/// does in a workspace that has never indexed tasks — and still without
/// opening the artifact payload.
#[test]
fn indexed_lexical_task_search_still_matches_comments_refs_and_artifact_paths() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let id = runtime
        .stores()
        .task_records()
        .create(TaskCreateParams {
            actor: "test".to_string(),
            parent_id: None,
            title: "sidecar surfaces stay searchable".to_string(),
            description: "ordinary body".to_string(),
            acceptance_criteria: Vec::new(),
            dependencies: Vec::new(),
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            plan: String::new(),
            execution_summary: String::new(),
            context_files: Vec::new(),
            repo_root: None,
            created_by: Some("test".to_string()),
            planned_by: None,
            implemented_by: None,
            status: TaskStatus::Backlog,
            priority: TaskPriority::Medium,
            complexity: None,
            task_type: TaskType::Chore,
            external_refs: vec![orbit_types::task::ExternalRef {
                system: orbit_types::task::GITHUB_PR_EXTERNAL_REF_SYSTEM.to_string(),
                id: "constellation-works/orbit#424242".to_string(),
                url: None,
            }],
            source_task_id: None,
            crew: None,
            orchestrator: None,
            comments: vec![orbit_types::task::TaskComment {
                at: chrono::Utc::now(),
                by: "test".to_string(),
                message: "reviewer asked about the quartzwren regression".to_string(),
            }],
        })
        .expect("create task")
        .id;
    runtime
        .stores()
        .task_artifacts()
        .upsert_task_artifacts(
            &id,
            orbit_store::TaskArtifactUpdateParams {
                origin: None,
                actor: "test".to_string(),
                owner_run_id: None,
                upsert_artifacts: vec![orbit_types::task::TaskArtifact::from_text(
                    "reports/lapisfinch-audit.md",
                    "body-only marmoset token\n",
                )],
            },
        )
        .expect("upsert artifact");
    let task = runtime.get_task(&id).expect("task with sidecars");
    let index = runtime
        .stores()
        .lexical_index()
        .store()
        .expect("lexical index");
    index.index_task(&task).expect("index task");
    assert!(
        index
            .has_source_kind(orbit_search::SOURCE_KIND_TASK)
            .expect("probe index"),
        "fixture must put the lexical branch on the FTS route"
    );

    let search = |query: &str| {
        runtime
            .global_search(GlobalSearchParams {
                query: Some(query.to_string()),
                kind: GlobalSearchKind::Task,
                ..Default::default()
            })
            .expect("lexical task search")
    };

    for (surface, needle) in [
        ("comment", "quartzwren"),
        ("external ref id", "orbit#424242"),
        ("artifact manifest path", "reports/lapisfinch-audit.md"),
        ("indexed title", "sidecar surfaces"),
    ] {
        let response = search(needle);
        assert_eq!(response.results.len(), 1, "{surface} needle {needle:?}");
        assert_eq!(response.results[0].source, "lexical", "{surface}");
        assert_eq!(
            response.results[0].id.as_deref(),
            Some(id.as_str()),
            "{surface}"
        );
    }

    assert!(
        search("marmoset").results.is_empty(),
        "artifact payloads stay outside interactive search"
    );
}

#[test]
fn lexical_task_search_notes_empty_multi_word_substring_miss() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let id = add_task(
        &runtime,
        "EnvGuard dropped its restore",
        "workspace_init lost the parallel env snapshot",
        TaskStatus::InProgress,
    );

    let miss = runtime
        .global_search(GlobalSearchParams {
            query: Some("EnvGuard workspace_init".to_string()),
            kind: GlobalSearchKind::Task,
            ..Default::default()
        })
        .expect("multi-word task search");

    assert!(
        miss.results.is_empty(),
        "non-contiguous terms are one needle"
    );
    assert!(
        miss.notes.iter().any(|note| {
            note.contains("single case-insensitive substring")
                && note.contains("not proof the corpus is empty")
                && note.contains("EnvGuard")
                && note.contains("workspace_init")
        }),
        "empty multi-word miss must carry a substring diagnostic, got {:?}",
        miss.notes
    );

    let hit = runtime
        .global_search(GlobalSearchParams {
            query: Some("EnvGuard".to_string()),
            kind: GlobalSearchKind::Task,
            ..Default::default()
        })
        .expect("single-term task search");

    assert_eq!(hit.results.len(), 1);
    assert_eq!(hit.results[0].id.as_deref(), Some(id.as_str()));
    assert!(
        hit.notes.is_empty(),
        "single-term hits must not attach the empty-query diagnostic: {:?}",
        hit.notes
    );
}

#[test]
fn friction_branch_searches_open_records_and_rejects_learning_kind() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    runtime
        .execute_tool_command(
            "orbit.friction.add",
            serde_json::json!({
                "title": "Heliotrope retry failure",
                "body": "The heliotrope retry path drops its terminal diagnostic.",
                "tags": ["tooling"],
                "model": "codex",
            }),
            Some("codex".to_string()),
            Some("codex".to_string()),
        )
        .expect("add friction fixture");

    let response = runtime
        .global_search(GlobalSearchParams {
            query: Some("heliotrope".to_string()),
            kind: GlobalSearchKind::Friction,
            tags: vec!["tooling".to_string()],
            limit: 5,
            ..Default::default()
        })
        .expect("search frictions");

    assert_eq!(response.results.len(), 1);
    assert_eq!(response.results[0].kind, "friction");
    assert_eq!(response.results[0].source, "lexical");
    assert!(GlobalSearchKind::from_str("learning").is_err());
}

#[test]
fn adr_search_kind_is_rejected() {
    let error = GlobalSearchKind::from_str("adr").expect_err("ADR corpus was retired");
    assert!(error.contains("expected one of: task, friction, all"));
}

#[test]
fn learning_search_kind_is_rejected() {
    let error = GlobalSearchKind::from_str("learning").expect_err("learning corpus was retired");
    assert!(error.contains("`learning`"));
    assert!(error.contains("expected one of: task, friction, all"));
}

#[test]
fn global_search_status_filter_requires_kind_prefix() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let error = runtime
        .global_search(GlobalSearchParams {
            query: Some("needle".to_string()),
            status: vec!["open".to_string()],
            ..Default::default()
        })
        .expect_err("bare status token should fail");

    assert!(error.to_string().contains("kind:value"));
}

/// [ORB-12259] `--path` search carries the skipped kinds as structured data,
/// not just prose in `notes`, so an agent reading an empty `results` array
/// does not mistake "frictions were never asked" for "nothing relevant exists".
#[test]
fn global_search_path_filter_lists_skipped_kinds_in_response() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");

    let response = runtime
        .global_search(GlobalSearchParams {
            kind: GlobalSearchKind::All,
            path: Some("src/check/rules.rs".to_string()),
            ..Default::default()
        })
        .expect("path search");

    assert_eq!(response.skipped_kinds, vec!["friction"]);

    let json = serde_json::to_value(&response).expect("serialize response");
    assert_eq!(json["skipped_kinds"], serde_json::json!(["friction"]));
}

/// A query that does not set `--path` never skips a kind, and the field must
/// stay absent rather than render as an empty array every caller now has to
/// special-case.
#[test]
fn global_search_without_path_omits_skipped_kinds_from_json() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let id = add_task_with_status(&runtime, "no path filter here", TaskStatus::Backlog);

    let response = runtime
        .global_search(GlobalSearchParams {
            query: Some("no path filter here".to_string()),
            kind: GlobalSearchKind::Task,
            ..Default::default()
        })
        .expect("plain query");

    assert!(response.skipped_kinds.is_empty());
    assert_eq!(response.results[0].id.as_deref(), Some(id.as_str()));
    let json = serde_json::to_value(&response).expect("serialize response");
    assert!(json.get("skipped_kinds").is_none());
}

#[test]
fn task_deletion_cascades_to_lexical_chunks() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let id = add_task(
        &runtime,
        "quartz routing telescope",
        "body",
        TaskStatus::Backlog,
    );
    assert!(runtime.search_index_stats().expect("stats").chunks > 0);
    assert!(
        runtime
            .stores()
            .task_records()
            .delete(&id)
            .expect("delete task")
    );
    assert_eq!(runtime.search_index_stats().expect("stats").chunks, 0);
    let index = runtime.stores().lexical_index().store().expect("index");
    assert!(
        bm25_top_k(index, "quartz telescope", Some(SOURCE_KIND_TASK), None, 5)
            .expect("query")
            .is_empty()
    );
}

/// Status filters apply before the candidate budget: when the strongest
/// lexical matches are all Done, the open match still fills the page.
#[test]
fn filtered_out_top_matches_do_not_starve_the_task_page() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    for index in 0..6 {
        add_task(
            &runtime,
            &format!("haystack haystack {index}"),
            "haystack haystack haystack",
            TaskStatus::Done,
        );
    }
    let open = add_task(
        &runtime,
        "unrelated title",
        "a long description that mentions the haystack only once in passing",
        TaskStatus::Backlog,
    );

    let response = runtime
        .global_search(GlobalSearchParams {
            query: Some("haystack".to_string()),
            kind: GlobalSearchKind::Task,
            limit: 1,
            ..Default::default()
        })
        .expect("search tasks");

    let ids: Vec<Option<&str>> = response
        .results
        .iter()
        .map(|hit| hit.id.as_deref())
        .collect();
    assert_eq!(ids, [Some(open.as_str())]);
}

/// Comments are invisible to FTS, so these matches come from the bundle
/// matcher that supplements the index. The scan must hand back exactly what a
/// full scan followed by the status and path filters would: newest first,
/// filtered from envelopes, and cut at the page.
#[test]
fn bundle_matcher_supplement_keeps_order_filters_and_page_size() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    // One indexed task so the lexical branch consults FTS before the matcher.
    add_task(&runtime, "anchor", "ordinary body", TaskStatus::Backlog);
    let mut open_matches = Vec::new();
    let mut all_matches = Vec::new();
    let mut hot_matches = Vec::new();
    for index in 0..10 {
        let done = index % 2 == 0;
        let id = add_commented_task(
            &runtime,
            &format!("commented {index}"),
            "note the sidecar-only-marker here",
            if done {
                TaskStatus::Done
            } else {
                TaskStatus::Backlog
            },
            &[if index % 3 == 0 {
                "file:src/hot.rs"
            } else {
                "file:src/cold.rs"
            }],
        );
        if !done {
            open_matches.push(id.clone());
        }
        if index % 3 == 0 {
            hot_matches.push(id.clone());
        }
        all_matches.push(id);
    }
    // Listing order is newest first.
    open_matches.reverse();
    all_matches.reverse();
    hot_matches.reverse();

    let search = |params: GlobalSearchParams| -> Vec<String> {
        runtime
            .global_search(GlobalSearchParams {
                query: Some("sidecar-only-marker".to_string()),
                kind: GlobalSearchKind::Task,
                ..params
            })
            .expect("search tasks")
            .results
            .into_iter()
            .filter_map(|hit| hit.id)
            .collect()
    };

    assert_eq!(
        search(GlobalSearchParams {
            limit: 3,
            ..Default::default()
        }),
        open_matches[..3],
        "default statuses hide done tasks, and the page keeps the newest"
    );
    assert_eq!(
        search(GlobalSearchParams {
            limit: 10,
            ..Default::default()
        }),
        open_matches
    );
    assert_eq!(
        search(GlobalSearchParams {
            limit: 4,
            all: true,
            ..Default::default()
        }),
        all_matches[..4]
    );
    assert_eq!(
        search(GlobalSearchParams {
            limit: 10,
            all: true,
            path: Some("src/hot.rs".to_string()),
            ..Default::default()
        }),
        hot_matches
    );
}

/// `--path` and `--tag` without a query judge every task from its envelope and
/// hydrate only the page.
#[test]
fn queryless_path_search_returns_the_newest_matching_page() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let mut expected = Vec::new();
    for index in 0..9 {
        let hot = index % 2 == 0;
        let id = add_commented_task(
            &runtime,
            &format!("routed {index}"),
            "note",
            TaskStatus::Backlog,
            &[if hot {
                "file:src/hot.rs"
            } else {
                "file:src/cold.rs"
            }],
        );
        if hot {
            expected.push(id);
        }
    }
    expected.reverse();

    let hits = |limit: usize| -> Vec<(String, Option<String>)> {
        runtime
            .global_search(GlobalSearchParams {
                path: Some("src/hot.rs".to_string()),
                kind: GlobalSearchKind::Task,
                limit,
                ..Default::default()
            })
            .expect("search by path")
            .results
            .into_iter()
            .map(|hit| (hit.id.expect("task id"), hit.title))
            .collect()
    };

    let page = hits(3);
    assert_eq!(
        page.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
        expected[..3]
    );
    assert!(
        page.iter().all(|(_, title)| title.is_some()),
        "hits are built from hydrated tasks"
    );
    assert_eq!(
        hits(50).into_iter().map(|(id, _)| id).collect::<Vec<_>>(),
        expected
    );
}

/// Twelve tasks whose title and description both repeat the query terms
/// outrank the one task that holds them apart, so the whole first BM25 page
/// belongs to tasks `rejected` shapes out of the filter.
const STARVING_QUERY: &str = "quartz telescope";

fn seed_filter_starvation(
    runtime: &OrbitRuntime,
    rejected: impl Fn(&mut TaskCreateParams),
    eligible: impl Fn(&mut TaskCreateParams),
) -> String {
    let create = |title: String, description: &str, shape: &dyn Fn(&mut TaskCreateParams)| {
        let mut params = TaskCreateParams {
            actor: "test".to_string(),
            parent_id: None,
            title,
            description: description.to_string(),
            acceptance_criteria: Vec::new(),
            dependencies: Vec::new(),
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            plan: String::new(),
            execution_summary: String::new(),
            context_files: Vec::new(),
            repo_root: None,
            created_by: Some("test".to_string()),
            planned_by: None,
            implemented_by: None,
            status: TaskStatus::Backlog,
            priority: TaskPriority::Medium,
            complexity: None,
            task_type: TaskType::Chore,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: None,
            orchestrator: None,
            comments: Vec::new(),
        };
        shape(&mut params);
        runtime
            .stores()
            .task_records()
            .create(params)
            .expect("create task")
            .id
    };
    for index in 0..12 {
        create(
            format!("quartz telescope quartz telescope {index}"),
            "quartz telescope",
            &rejected,
        );
    }
    let eligible = create(
        "quartz routing telescope".to_string(),
        "ordinary body",
        &eligible,
    );

    let index = runtime.stores().lexical_index().store().expect("index");
    let ranking =
        bm25_top_k(index, STARVING_QUERY, Some(SOURCE_KIND_TASK), None, 100).expect("full ranking");
    let position = ranking
        .iter()
        .position(|hit| hit.source_id == eligible)
        .expect("FTS matches the non-adjacent terms");
    assert!(
        position >= 10,
        "fixture must bury the eligible task beneath the first ten chunks, found at {position}"
    );
    eligible
}

fn search_one(runtime: &OrbitRuntime, params: GlobalSearchParams) -> Vec<String> {
    runtime
        .global_search(GlobalSearchParams {
            query: Some(STARVING_QUERY.to_string()),
            kind: GlobalSearchKind::Task,
            limit: 1,
            ..params
        })
        .expect("search tasks")
        .results
        .into_iter()
        .filter_map(|hit| hit.id)
        .collect()
}

/// The bundle matcher reads `quartz telescope` as one contiguous needle and
/// misses `quartz routing telescope`, so only BM25 paging past the
/// filtered-out first page can find the eligible task.
#[test]
fn status_filter_pages_bm25_past_rejected_chunks() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let eligible =
        seed_filter_starvation(&runtime, |params| params.status = TaskStatus::Done, |_| {});

    assert_eq!(
        search_one(&runtime, GlobalSearchParams::default()),
        [eligible]
    );
}

#[test]
fn tag_filter_pages_bm25_past_rejected_chunks() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let eligible = seed_filter_starvation(
        &runtime,
        |_| {},
        |params| params.tags = vec!["optics".to_string()],
    );

    let params = GlobalSearchParams {
        tags: vec!["optics".to_string()],
        ..Default::default()
    };
    assert_eq!(search_one(&runtime, params), [eligible]);
}

#[test]
fn path_filter_pages_bm25_past_rejected_chunks() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let eligible = seed_filter_starvation(
        &runtime,
        |params| params.context_files = vec!["file:src/other.rs".to_string()],
        |params| params.context_files = vec!["file:src/optics/router.rs".to_string()],
    );

    let params = GlobalSearchParams {
        path: Some("src/optics/router.rs".to_string()),
        ..Default::default()
    };
    assert_eq!(search_one(&runtime, params), [eligible]);
}

/// Consecutive bounded pages reproduce the single ranked query hit for hit,
/// including the absolute rank, so paging cannot reorder or drop matches.
#[test]
fn bm25_pages_concatenate_to_the_top_k_ranking() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    seed_filter_starvation(&runtime, |_| {}, |_| {});
    let index = runtime.stores().lexical_index().store().expect("index");

    let ranking =
        bm25_top_k(index, STARVING_QUERY, Some(SOURCE_KIND_TASK), None, 100).expect("full ranking");
    let mut paged = Vec::new();
    for offset in (0..).step_by(7) {
        let page = bm25_page(
            index,
            STARVING_QUERY,
            Some(SOURCE_KIND_TASK),
            None,
            offset,
            7,
        )
        .expect("page");
        assert!(page.len() <= 7, "a page never exceeds its limit");
        let last = page.len() < 7;
        paged.extend(page);
        if last {
            break;
        }
    }

    assert!(ranking.len() > 14, "fixture spans several pages");
    assert_eq!(paged, ranking);
}
