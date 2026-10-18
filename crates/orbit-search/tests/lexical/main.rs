//! Query ranking and token semantics through the public SQLite index API.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use orbit_search::{LexicalIndex, bm25_or_page, bm25_page, bm25_top_k};
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
        origin: None,
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

#[test]
fn writes_updates_deletes_and_rebuilds_fts_synchronously() {
    let dir = tempfile::tempdir().expect("fixture directory");
    let index = LexicalIndex::open(&dir.path().join("search.db")).expect("open");
    let store = index.store().expect("store");
    let mut record = task("task-a", "cobalt routing observatory");
    store.index_task(&record).expect("index");
    let hits = bm25_top_k(store, "cobalt observatory", Some("task"), None, 10).expect("query");
    assert_eq!(hits[0].source_id, record.id);
    record.title = "quartz routing telescope".into();
    store.index_task(&record).expect("update");
    assert!(
        bm25_top_k(store, "cobalt", None, None, 10)
            .expect("query")
            .is_empty()
    );
    assert_eq!(
        bm25_top_k(store, "quartz telescope", None, None, 10)
            .expect("query")
            .len(),
        1
    );
    store.delete_source("task", &record.id).expect("delete");
    assert!(
        bm25_top_k(store, "quartz", None, None, 10)
            .expect("query")
            .is_empty()
    );
    store
        .index_task(&task("stale", "stale source"))
        .expect("stale");
    let report = store.reindex_tasks(&[record]).expect("rebuild");
    assert_eq!((report.tasks, report.chunks), (1, 1));
    assert!(
        bm25_top_k(store, "stale", None, None, 10)
            .expect("query")
            .is_empty()
    );
}

#[test]
fn first_open_migrates_legacy_vectors_and_preserves_search() {
    let dir = tempfile::tempdir().expect("fixture directory");
    let path = dir.path().join("semantic.db");
    let conn = rusqlite::Connection::open(&path).expect("fixture database");
    conn.execute_batch("CREATE TABLE embeddings (source_id TEXT, embedding BLOB);
        CREATE INDEX embeddings_by_source ON embeddings(source_id);
        INSERT INTO embeddings VALUES ('task-a', zeroblob(1048576));
        CREATE TABLE id_allocations (id INTEGER);
        CREATE VIRTUAL TABLE corpus_fts USING fts5(source_kind UNINDEXED, source_id UNINDEXED, field UNINDEXED, content);
        INSERT INTO corpus_fts VALUES ('task', 'task-a', 'title', 'cobalt routing observatory');")
        .expect("legacy schema");
    let before = std::fs::metadata(&path).expect("metadata").len();
    drop(conn);
    let index = LexicalIndex::open(&path).expect("migrate");
    assert_eq!(
        bm25_top_k(
            index.store().expect("store"),
            "cobalt observatory",
            None,
            None,
            5
        )
        .expect("search")[0]
            .source_id,
        "task-a"
    );
    drop(index);
    let conn = rusqlite::Connection::open(&path).expect("migrated database");
    for table in ["embeddings", "id_allocations"] {
        let exists: i64 = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                [table],
                |row| row.get(0),
            )
            .expect("table");
        assert_eq!(exists, 0);
    }
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .expect("checkpoint");
    assert!(std::fs::metadata(&path).expect("metadata").len() < before);
    drop(conn);
    let reopened = LexicalIndex::open(&path).expect("idempotent reopen");
    assert_eq!(
        reopened
            .store()
            .expect("store")
            .stats()
            .expect("stats")
            .tasks,
        1
    );
}
