//! Unit tests for `pool` — the host's per-model companion cache.
//!
//! Every test counts spawns through a fake spawner, so what is asserted is the
//! thing the companion actually costs: how many times a model was loaded.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};

use crate::vector::VectorStore;
use crate::{
    Embedder, EmbedderPool, NoopEmbedder, SemanticRelatedParams, SemanticSearchParams,
    semantic_related, semantic_search,
};

/// A pool whose companions are `NoopEmbedder`s, plus the spawn counter.
fn counting_pool() -> (EmbedderPool, Arc<AtomicUsize>) {
    let spawns = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&spawns);
    let pool = EmbedderPool::with_spawner(move |_model| {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(NoopEmbedder::small()) as Arc<dyn Embedder>)
    });
    (pool, spawns)
}

fn task(id: &str, title: &str) -> Task {
    Task {
        job_run_machine: None,
        id: id.to_string(),
        title: title.to_string(),
        description: "Reused embedder coverage".to_string(),
        acceptance_criteria: Vec::new(),
        tags: Vec::new(),
        required_tools: Vec::new(),
        plan: String::new(),
        execution_summary: String::new(),
        context_files: Vec::new(),
        created_by: None,
        planned_by: None,
        implemented_by: None,
        status: TaskStatus::Backlog,
        priority: TaskPriority::Medium,
        complexity: None,
        task_type: TaskType::Chore,
        pr_status: None,
        external_refs: Vec::new(),
        relations: Vec::new(),
        job_run_id: None,
        crew: None,
        orchestrator: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

/// A store holding one indexed task under the `noop` model.
fn indexed_store() -> VectorStore {
    let store = VectorStore::open_in_memory().unwrap();
    store
        .reindex_tasks(
            &[task("ORB-00001", "Reuse the companion")],
            &NoopEmbedder::small(),
            false,
        )
        .unwrap();
    store
}

fn search_params(query: &str) -> SemanticSearchParams {
    SemanticSearchParams {
        query: query.to_string(),
        limit: 5,
        field: None,
        kind: None,
        model: None,
    }
}

#[test]
fn two_consecutive_queries_reuse_one_embedder() {
    let store = indexed_store();
    let (pool, spawns) = counting_pool();

    semantic_search(&store, &pool, search_params("first query")).unwrap();
    semantic_search(&store, &pool, search_params("second query")).unwrap();

    assert_eq!(
        spawns.load(Ordering::SeqCst),
        1,
        "the second query must answer from the companion the first one started"
    );
}

#[test]
fn neighbor_lookup_reuses_the_query_embedder() {
    let store = indexed_store();
    let (pool, spawns) = counting_pool();
    let target = task("ORB-00001", "Reuse the companion");

    semantic_search(&store, &pool, search_params("warm the pool")).unwrap();
    semantic_related(
        &store,
        &target,
        &pool,
        SemanticRelatedParams {
            task_id: "ORB-00001".to_string(),
            limit: 5,
            model: None,
        },
    )
    .unwrap();

    assert_eq!(spawns.load(Ordering::SeqCst), 1);
}

#[test]
fn a_long_lived_host_shares_one_pool_across_every_runtime_it_opens() {
    // The MCP server opens a runtime per tool call, so the pool a long-lived
    // host installs has to outlive any one runtime.
    assert!(Arc::ptr_eq(
        &EmbedderPool::process_shared(),
        &EmbedderPool::process_shared()
    ));
}

#[test]
fn each_model_gets_its_own_embedder() {
    let (pool, spawns) = counting_pool();

    pool.embedder("bge-small").unwrap();
    pool.embedder("minilm-l6").unwrap();
    pool.embedder("bge-small").unwrap();

    assert_eq!(
        spawns.load(Ordering::SeqCst),
        2,
        "models are cached independently; only the repeat is free"
    );
}

#[test]
fn clear_drops_cached_companions_so_the_next_request_respawns() {
    let (pool, spawns) = counting_pool();

    pool.embedder("bge-small").unwrap();
    pool.clear();
    pool.embedder("bge-small").unwrap();

    assert_eq!(
        spawns.load(Ordering::SeqCst),
        2,
        "a replaced companion binary must not keep answering from the pool"
    );
}

#[test]
fn a_failed_spawn_is_not_cached() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let pool = EmbedderPool::with_spawner(move |_model| {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(OrbitError::Execution("companion not installed".to_string()));
        }
        Ok(Arc::new(NoopEmbedder::small()) as Arc<dyn Embedder>)
    });

    assert!(
        pool.embedder("bge-small").is_err(),
        "the first attempt reports the spawn failure"
    );
    assert!(
        pool.embedder("bge-small").is_ok(),
        "a later request retries rather than replaying the failure"
    );

    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}
