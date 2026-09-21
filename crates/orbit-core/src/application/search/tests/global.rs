use std::str::FromStr;

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
