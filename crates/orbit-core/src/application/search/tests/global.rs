use std::str::FromStr;

use super::*;

#[test]
fn global_search_all_round_robins_across_two_corpora() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let query = "threecorpora";
    seed_search_fixture(&runtime, query, 12, 12);

    let response = runtime
        .global_search(GlobalSearchParams {
            query: Some(query.to_string()),
            kind: GlobalSearchKind::All,
            limit: 6,
            ..Default::default()
        })
        .expect("search all");

    assert_eq!(response.results.len(), 6);
    for kind in ["task", "doc"] {
        assert_eq!(count_kind(&response.results, kind), 3, "{kind} count");
    }
    assert_eq!(count_kind(&response.results, "adr"), 0);
}

#[test]
fn global_search_single_kind_limit_keeps_task_behavior() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let query = "taskonly";
    seed_search_fixture(&runtime, query, 20, 3);

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
fn doc_branch_searches_inlined_adr_body_content() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    add_doc_with_body(
        &runtime,
        "docs/design/example/4_decisions.md",
        "Example decisions",
        "## ADR-0999 — Inline test\n\n### Decision\n\nUse the heliotrope-dispatch invariant.",
    );

    let response = runtime
        .global_search(GlobalSearchParams {
            query: Some("heliotrope-dispatch".to_string()),
            kind: GlobalSearchKind::Doc,
            limit: 5,
            ..Default::default()
        })
        .expect("search docs");

    assert_eq!(response.results.len(), 1);
    let hit = &response.results[0];
    assert_eq!(hit.kind, "doc");
    assert_eq!(
        hit.path.as_deref(),
        Some("docs/design/example/4_decisions.md")
    );
    assert!(
        hit.matched_by
            .as_deref()
            .is_some_and(|fields| fields.contains(&"body".to_string()))
    );
    assert!(
        hit.snippet
            .as_deref()
            .is_some_and(|snippet| snippet.contains("heliotrope-dispatch"))
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
    assert!(error.contains("expected one of: task, doc, friction, all"));
}

#[test]
fn learning_search_kind_is_rejected() {
    let error = GlobalSearchKind::from_str("learning").expect_err("learning corpus was retired");
    assert!(error.contains("`learning`"));
    assert!(error.contains("expected one of: task, doc, friction, all"));
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

#[test]
fn global_search_path_filter_notes_doc_branch_skip() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    add_doc(&runtime, "docs/path-note.md", "needle path note");

    let response = runtime
        .global_search(GlobalSearchParams {
            kind: GlobalSearchKind::All,
            path: Some("crates/orbit-cli/".to_string()),
            ..Default::default()
        })
        .expect("path search");

    assert!(
        response
            .notes
            .iter()
            .any(|note| note.contains("doc branch skipped"))
    );
}
