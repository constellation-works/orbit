use super::*;
use serde_json::json;

#[test]
fn global_search_task_hybrid_preserves_retriever_breakdown() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let id = add_task_with_status(&runtime, "task hybrid breakdown", TaskStatus::Backlog);

    let response = with_task_semantic_override(Ok(vec![task_semantic_hit(&id, 0.75)]), || {
        runtime
            .global_search(GlobalSearchParams {
                query: Some("task hybrid breakdown".to_string()),
                hybrid: true,
                kind: GlobalSearchKind::Task,
                limit: 3,
                ..Default::default()
            })
            .expect("task hybrid search")
    });

    assert_eq!(response.mode, GlobalSearchMode::Hybrid);
    assert_eq!(
        serde_json::to_value(&response.results[0]).expect("serialize hit"),
        json!({
            "kind": "task",
            "source": "semantic",
            "id": id,
            "status": "backlog",
            "best_field": "title",
            "snippet": "semantic task snippet",
            "score": 0.75,
            "score_breakdown": {
                "rrf": 0.75,
                "bm25_rank": 2,
                "cosine_rank": 1
            }
        })
    );
}

#[test]
fn global_search_task_hybrid_falls_back_to_lexical_on_semantic_error() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let id = add_task_with_status(&runtime, "task fallback needle", TaskStatus::Backlog);

    let response = with_task_semantic_override(
        Err(OrbitError::Execution(
            "companion startup failed".to_string(),
        )),
        || {
            runtime
                .global_search(GlobalSearchParams {
                    query: Some("task fallback needle".to_string()),
                    hybrid: true,
                    kind: GlobalSearchKind::Task,
                    limit: 3,
                    ..Default::default()
                })
                .expect("task lexical fallback")
        },
    );

    assert_eq!(response.mode, GlobalSearchMode::Lexical);
    assert!(response.notes.iter().any(|note| {
        note.contains("falling back to lexical task search")
            && note.contains("companion startup failed")
    }));
    assert_eq!(response.results[0].source, "lexical");
    assert_eq!(response.results[0].id.as_deref(), Some(id.as_str()));
}

#[test]
fn global_search_task_hybrid_falls_back_without_install_remediation() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let id = add_task_with_status(
        &runtime,
        "task absent companion needle",
        TaskStatus::Backlog,
    );

    let response = with_task_semantic_override(
        Err(OrbitError::CompanionNotInstalled(
            orbit_search::INSTALL_REMEDIATION.to_string(),
        )),
        || {
            runtime
                .global_search(GlobalSearchParams {
                    query: Some("task absent companion needle".to_string()),
                    hybrid: true,
                    kind: GlobalSearchKind::Task,
                    limit: 1,
                    ..Default::default()
                })
                .expect("missing companion should use lexical search")
        },
    );

    assert_eq!(response.mode, GlobalSearchMode::Lexical);
    assert_eq!(response.results[0].source, "lexical");
    assert_eq!(response.results[0].id.as_deref(), Some(id.as_str()));
    assert!(response.notes.iter().any(|note| {
        note.contains("falling back to lexical task search")
            && note.contains("optional inference companion unavailable")
    }));
    assert!(
        response
            .notes
            .iter()
            .all(|note| !note.contains("orbit semantic install"))
    );
}

#[test]
fn global_search_task_hybrid_layout_incompatibility_is_lexical_fallback_not_hybrid_success() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let id = add_task_with_status(&runtime, "layout mismatch needle", TaskStatus::Backlog);

    let response = with_task_semantic_override(
        Err(OrbitError::Store(
            "semantic index layout is incompatible with this Orbit runtime: \
             corpus_fts is an external-content FTS5 table over chunks and has no \
             source_kind column. Upgrade and restart every Orbit process that \
             opens this workspace's .orbit/state/semantic.db, including Homebrew \
             installs and MCP servers."
                .to_string(),
        )),
        || {
            runtime
                .global_search(GlobalSearchParams {
                    query: Some("layout mismatch needle".to_string()),
                    hybrid: true,
                    kind: GlobalSearchKind::Task,
                    limit: 3,
                    ..Default::default()
                })
                .expect("layout mismatch should keep lexical search")
        },
    );

    assert_eq!(
        response.mode,
        GlobalSearchMode::Lexical,
        "layout mismatch is lexical fallback, not hybrid success: notes={:?}",
        response.notes
    );
    assert!(response.notes.iter().any(|note| {
        note.contains("falling back to lexical task search")
            && note.contains("semantic index layout is incompatible with this Orbit runtime")
            && note.contains("Upgrade and restart every Orbit process")
    }));
    assert!(
        response
            .notes
            .iter()
            .all(|note| !note.to_ascii_lowercase().contains("no such column")),
        "fallback notes must not leak the unexplained SQL-column error: {:?}",
        response.notes
    );
    assert_eq!(response.results[0].source, "lexical");
    assert_eq!(response.results[0].id.as_deref(), Some(id.as_str()));
}

#[test]
fn global_search_doc_hybrid_uses_docs_semantic_weight() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    add_doc_with_tags(&runtime, "docs/z-lexical.md", "Literal primary", &["foo"]);
    add_doc(&runtime, "docs/y-lexical.md", "foo secondary");
    add_doc(&runtime, "docs/a-semantic.md", "Conceptual match");
    let semantic = vec![
        doc_semantic_hit("docs/a-semantic.md", 1.0),
        doc_semantic_hit("docs/y-lexical.md", 0.2),
    ];

    let top_path = |weight: f32| {
        fs::write(
            runtime.config_path().expect("resolve config path"),
            format!("[docs.search]\nsemantic_weight = {weight:.1}\n"),
        )
        .expect("write config");
        with_doc_semantic_override(Ok(semantic.clone()), || {
            runtime
                .global_search(GlobalSearchParams {
                    query: Some("foo".to_string()),
                    hybrid: true,
                    kind: GlobalSearchKind::Doc,
                    limit: 3,
                    ..Default::default()
                })
                .expect("doc hybrid search")
                .results
                .into_iter()
                .next()
                .expect("top result")
                .path
                .expect("doc path")
        })
    };

    assert_eq!(top_path(0.0), "docs/z-lexical.md");
    assert_eq!(top_path(1.0), "docs/a-semantic.md");
    assert_eq!(top_path(0.5), "docs/a-semantic.md");
}

#[test]
fn global_search_doc_hybrid_falls_back_to_lexical_on_semantic_error() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    add_doc_with_tags(
        &runtime,
        "docs/fallback-z-lexical.md",
        "Fallback primary",
        &["fallbackneedle"],
    );

    let response = with_doc_semantic_override(
        Err(OrbitError::Execution(
            "companion startup failed".to_string(),
        )),
        || {
            runtime
                .global_search(GlobalSearchParams {
                    query: Some("fallbackneedle".to_string()),
                    hybrid: true,
                    kind: GlobalSearchKind::Doc,
                    limit: 3,
                    ..Default::default()
                })
                .expect("fallback search")
        },
    );

    assert!(
        response
            .notes
            .iter()
            .any(|note| note.contains("falling back to lexical"))
    );
    assert_eq!(response.results[0].source, "lexical");
    assert_eq!(
        response.results[0].path.as_deref(),
        Some("docs/fallback-z-lexical.md")
    );
}

#[test]
fn doc_hybrid_fallback_preserves_lexical_filtering_and_order() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    add_doc_with_tags(
        &runtime,
        "docs/parity-a.md",
        "parity needle first",
        &["keep"],
    );
    add_doc_with_tags(
        &runtime,
        "docs/parity-b.md",
        "parity needle second",
        &["keep"],
    );
    add_doc_with_tags(
        &runtime,
        "docs/parity-c.md",
        "parity needle filtered",
        &["drop"],
    );

    let params = GlobalSearchParams {
        query: Some("parity needle".to_string()),
        kind: GlobalSearchKind::Doc,
        tags: vec!["keep".to_string()],
        limit: 2,
        ..Default::default()
    };
    let lexical = runtime
        .global_search(params.clone())
        .expect("lexical search");
    let fallback = with_doc_semantic_override(
        Err(OrbitError::Execution(
            "companion startup failed".to_string(),
        )),
        || {
            runtime
                .global_search(GlobalSearchParams {
                    hybrid: true,
                    ..params
                })
                .expect("lexical fallback")
        },
    );

    let paths = |response: &GlobalSearchResponse| {
        response
            .results
            .iter()
            .map(|hit| hit.path.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(paths(&fallback), paths(&lexical));
    assert_eq!(fallback.results.len(), 2);
    assert!(
        fallback
            .results
            .iter()
            .all(|hit| hit.path.as_deref() != Some("docs/parity-c.md"))
    );
}

#[test]
fn hybrid_handles_single_candidate_side() {
    let hit = GlobalSearchHit {
        kind: "doc".to_string(),
        source: "hybrid".to_string(),
        id: None,
        path: Some("docs/only.md".to_string()),
        title: None,
        summary: None,
        status: None,
        best_field: None,
        snippet: None,
        score: None,
        score_breakdown: None,
        matched_by: None,
        workspace: None,
    };
    let out = blend_doc_hybrid_candidates(
        vec![DocHybridCandidate {
            hit,
            lexical_score: Some(0.42),
            semantic_score: None,
            semantic: None,
        }],
        0.5,
    );

    assert!((out[0].score.expect("score") - 0.21).abs() < 0.0001);
}
