//! Query ranking and token semantics through the public SQLite index API.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use orbit_search::{LexicalIndex, bm25_or_page, bm25_page};
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};

fn task(id: &str, title: &str) -> Task {
    let at = "2026-09-21T00:00:00Z".parse().expect("timestamp");
    Task {
        id: id.into(),
        title: title.into(),
        description: String::new(),
        acceptance_criteria: vec![],
        tags: vec![],
        required_tools: vec![],
        plan: String::new(),
        execution_summary: String::new(),
        context_files: vec![],
        created_by: None,
        planned_by: None,
        implemented_by: None,
        status: TaskStatus::Backlog,
        priority: TaskPriority::Medium,
        complexity: None,
        task_type: TaskType::Chore,
        pr_status: None,
        external_refs: vec![],
        relations: vec![],
        job_run_id: None,
        job_run_machine: None,
        crew: None,
        crew_source: None,
        orchestrator: None,
        created_at: at,
        updated_at: at,
    }
}

#[test]
fn any_term_pages_rank_coverage_then_bm25_with_fts_token_counts() {
    let dir = tempfile::tempdir().unwrap();
    let index = LexicalIndex::open(&dir.path().join("search.db")).unwrap();
    let store = index.store().unwrap();
    // Equal coverage: the shorter chunk wins BM25 even though it was inserted
    // later. A substring false positive never enters the OR ranking.
    for (id, title) in [
        (
            "long",
            "generation participant mcp unrelated words pad this chunk",
        ),
        ("short", "generation participant mcp"),
        ("most", "generation participant mcp serve pinned"),
        ("one", "deployed participantish generationless deploy"),
        ("none", "generationless participantish"),
    ] {
        store.index_task(&task(id, title)).unwrap();
    }
    let query = "generation participant mcp serve pinned deploy";
    assert!(
        bm25_page(store, query, Some("task"), None, 0, 10)
            .unwrap()
            .is_empty()
    );
    let page = bm25_or_page(store, query, Some("task"), None, 0, 10).unwrap();
    assert_eq!(
        page.iter()
            .map(|hit| (hit.source_id.as_str(), hit.matched_terms))
            .collect::<Vec<_>>(),
        vec![("most", 5), ("short", 3), ("long", 3), ("one", 1)]
    );
    let first = bm25_or_page(store, query, Some("task"), None, 0, 2).unwrap();
    let second = bm25_or_page(store, query, Some("task"), None, 2, 2).unwrap();
    assert_eq!(
        [first, second].concat(),
        page,
        "coverage and BM25 order is stable across pages"
    );

    store
        .index_task(&task("normalized", "Café MCP-SERVE"))
        .unwrap();
    let page = bm25_or_page(store, "cafe mcp-serve missing", Some("task"), None, 0, 10).unwrap();
    assert_eq!(
        (page[0].source_id.as_str(), page[0].matched_terms),
        ("normalized", 2),
        "counts use FTS diacritic normalization and quoted phrase semantics"
    );
    assert!(
        bm25_or_page(store, query, Some("friction"), None, 0, 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        bm25_or_page(store, query, Some("task"), Some("plan"), 0, 10)
            .unwrap()
            .is_empty()
    );
}
