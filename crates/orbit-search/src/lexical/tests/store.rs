//! Persistence and query behavior at the SQLite boundary.
use super::super::*;
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
        orchestrator: None,
        created_at: at,
        updated_at: at,
    }
}

#[test]
fn writes_updates_deletes_and_rebuilds_fts_synchronously() {
    let store = LexicalStore::open_in_memory().expect("open");
    let mut record = task("task-a", "cobalt routing observatory");
    store.index_task(&record).expect("index");
    let hits = bm25_top_k(&store, "cobalt observatory", Some("task"), None, 10).expect("query");
    assert_eq!(hits[0].source_id, record.id);
    record.title = "quartz routing telescope".into();
    store.index_task(&record).expect("update");
    assert!(
        bm25_top_k(&store, "cobalt", None, None, 10)
            .expect("query")
            .is_empty()
    );
    assert_eq!(
        bm25_top_k(&store, "quartz telescope", None, None, 10)
            .expect("query")
            .len(),
        1
    );
    store.delete_source("task", &record.id).expect("delete");
    assert!(
        bm25_top_k(&store, "quartz", None, None, 10)
            .expect("query")
            .is_empty()
    );
    store
        .index_task(&task("stale", "stale source"))
        .expect("stale");
    let report = store.reindex_tasks(&[record]).expect("rebuild");
    assert_eq!((report.tasks, report.chunks), (1, 1));
    assert!(
        bm25_top_k(&store, "stale", None, None, 10)
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
    let store = LexicalStore::open(&path).expect("migrate");
    assert_eq!(
        bm25_top_k(&store, "cobalt observatory", None, None, 5).expect("search")[0].source_id,
        "task-a"
    );
    let conn = store.connection();
    let conn = conn.lock().expect("lock");
    assert!(!migration::table_exists(&conn, "embeddings").expect("table"));
    assert!(!migration::table_exists(&conn, "id_allocations").expect("table"));
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .expect("checkpoint");
    assert!(std::fs::metadata(&path).expect("metadata").len() < before);
    drop(conn);
    drop(store);
    let reopened = LexicalStore::open(&path).expect("idempotent reopen");
    assert_eq!(reopened.stats().expect("stats").tasks, 1);
}

#[test]
fn word_chunker_is_bounded_and_preserves_unicode_and_overlap() {
    let text = (0..600)
        .map(|n| format!("étoile{n}"))
        .collect::<Vec<_>>()
        .join(" ");
    let chunks = chunker::chunk_text(&text);
    assert!(
        chunks
            .iter()
            .all(|chunk| chunk.split_whitespace().count() <= 256)
    );
    assert!(chunks.first().expect("first").starts_with("étoile0 "));
    assert!(chunks.last().expect("last").ends_with("étoile599"));
    assert!(chunks[0].contains("étoile224") && chunks[1].starts_with("étoile224 "));
    assert!(chunker::chunk_text(" \n\n ").is_empty());
}
