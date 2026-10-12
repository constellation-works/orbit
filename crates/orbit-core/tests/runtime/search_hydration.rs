//! Lexical search hydrates its hits from the task documents alone: a cold
//! search over a workspace whose matching bundles carry megabytes of
//! artifacts neither reads nor verifies those payloads. Friction search reads
//! bounded pages and stops after its matching truncation lookahead.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use orbit_common::test_env::{self, FixtureProgress};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{GlobalSearchKind, GlobalSearchParams, OrbitError, OrbitRuntime};
use orbit_store::friction_store::{FrictionAddParams, FrictionStore};
use serde_json::json;
use tempfile::TempDir;
use tracing_subscriber::{Layer, layer::Context};

/// Tasks in the workspace; only [`MATCHING`] of them carry the query term.
const TASKS: usize = 40;
/// Matching tasks, each carrying one [`ARTIFACT_BYTES`] artifact: 20 MB.
const MATCHING: usize = 20;
const ARTIFACT_BYTES: usize = 1_000_000;
const TERM: &str = "quasarbeacon";

/// Deterministic store order, independent of allocation time or ID ordering.
fn seed_frictions(runtime: &OrbitRuntime, tags: &[Vec<String>]) -> (FrictionStore, Vec<String>) {
    let store = FrictionStore::open(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
        runtime.data_root().join("frictions"),
    )
    .unwrap();
    let ids = tags
        .iter()
        .enumerate()
        .map(|(index, tags)| {
            store
                .add(FrictionAddParams {
                    model: "codex".into(),
                    title: Some(format!("Fixture {index} {TERM}")),
                    body: format!("Friction body {index}"),
                    tags: tags.clone(),
                    status: orbit_types::record::FrictionStatus::Open,
                    during_task: None,
                    created_at: chrono::DateTime::from_timestamp(1_800_000_000 + index as i64, 0)
                        .unwrap(),
                })
                .unwrap()
                .record
                .id
        })
        .collect();
    (store, ids)
}

/// A read beyond the expected pages must fail decoding, rather than silently
/// passing a result-count assertion while hydrating the entire corpus.
fn poison_friction(runtime: &OrbitRuntime, id: &str) {
    let conn = rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
    assert_eq!(
        conn.execute(
            "UPDATE friction_records SET tags_json = '[1]' \
             WHERE workspace_id = ?1 AND friction_id = ?2",
            rusqlite::params![runtime.workspace_id().unwrap(), id],
        )
        .unwrap(),
        1
    );
}

#[test]
fn friction_search_and_listing_bound_store_reads() {
    if !super::dispatch_admission::isolated(
        "search_hydration::friction_search_and_listing_bound_store_reads",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();
    let runtime = OrbitRuntime::from_roots(
        &root.path().join("global"),
        &root.path().join("repo/.orbit"),
    )
    .unwrap();
    let (store, ids) = seed_frictions(&runtime, &vec![vec!["tooling".into()]; 8]);
    // Limit two plus one lookahead may decode only the first three records.
    poison_friction(&runtime, &ids[3]);
    assert!(
        store.show(&ids[3]).is_err(),
        "negative control: corrupt row"
    );
    for query in [None, Some(TERM.to_string())] {
        let listing = query.is_none();
        let response = runtime
            .global_search(GlobalSearchParams {
                query: query.clone(),
                kind: GlobalSearchKind::Friction,
                limit: 2,
                ..Default::default()
            })
            .unwrap();
        let found: Vec<_> = response
            .results
            .iter()
            .map(|hit| hit.id.as_ref().unwrap())
            .collect();
        assert_eq!(found, ids[..2].iter().collect::<Vec<_>>());
        assert_eq!(
            !response.notes.is_empty(),
            listing,
            "listing reports truncation"
        );
        for (index, hit) in response.results.iter().enumerate() {
            assert_eq!(
                hit.snippet.as_deref(),
                Some(format!("Friction body {index}").as_str())
            );
            assert_eq!(hit.record.is_some(), listing);
        }
        assert!(
            runtime
                .global_search(GlobalSearchParams {
                    query,
                    kind: GlobalSearchKind::Friction,
                    limit: 3,
                    ..Default::default()
                })
                .is_err(),
            "widening the page reaches the corrupt lookahead"
        );
    }
}

#[test]
fn friction_tag_pages_fill_limit_and_check_matching_lookahead() {
    if !super::dispatch_admission::isolated(
        "search_hydration::friction_tag_pages_fill_limit_and_check_matching_lookahead",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();
    let runtime = OrbitRuntime::from_roots(
        &root.path().join("global"),
        &root.path().join("repo/.orbit"),
    )
    .unwrap();
    let mut tags = vec![vec!["tooling".into()]; 14];
    for index in [4, 7, 8] {
        tags[index].push("other".into());
    }
    let (store, ids) = seed_frictions(&runtime, &tags);
    let search = |limit, tags| {
        runtime.global_search(GlobalSearchParams {
            kind: GlobalSearchKind::Friction,
            tags,
            limit,
            ..Default::default()
        })
    };
    let both_tags = vec!["TOOLING".into(), "OTHER".into()];
    for limit in [2, 3, 4] {
        let response = search(limit, both_tags.clone()).unwrap();
        let found: Vec<_> = response
            .results
            .iter()
            .map(|hit| hit.id.as_ref().unwrap())
            .collect();
        let expected: Vec<_> = [4, 7, 8]
            .iter()
            .take(limit)
            .map(|index| &ids[*index])
            .collect();
        assert_eq!(
            found, expected,
            "all tags match across pages, in store order"
        );
        assert_eq!(
            !response.notes.is_empty(),
            limit == 2,
            "only excess matching records truncate"
        );
    }
    let rejected = search(2, vec!["absent".into()]).unwrap();
    assert!(rejected.results.is_empty());
    assert!(rejected.notes.is_empty());
    // Three pages of three rows reach the third match at index eight. The
    // fourth page must not be decoded after that matching lookahead.
    poison_friction(&runtime, &ids[9]);
    assert!(
        store.show(&ids[9]).is_err(),
        "negative control: corrupt tail"
    );
    let response = search(2, both_tags).unwrap();
    assert_eq!(response.results.len(), 2);
    assert!(!response.notes.is_empty());
}

#[test]
fn cold_search_hits_skip_artifact_payloads() {
    if !super::dispatch_admission::isolated(
        "search_hydration::cold_search_hits_skip_artifact_payloads",
    ) {
        return;
    }
    // Keep the seed bounded: payload corruption proves the hydration contract
    // without thousands of fsyncs or a machine-dependent CPU-time ceiling.
    let root = tempfile::tempdir_in(test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();

    let mut progress = FixtureProgress::start("cold search");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let scratch = workspace.join("tmp");
    std::fs::create_dir_all(&scratch).unwrap();
    let mut matching_ids = Vec::new();
    progress.phase("seed tasks", TASKS);
    for index in 0..TASKS {
        let matching = index < MATCHING;
        let title = if matching {
            format!("Fixture {index} {TERM}")
        } else {
            format!("Fixture {index} unrelated")
        };
        let task = runtime
            .add_task(TaskAddParams {
                title,
                description: "Seeded search fixture.".into(),
                ..Default::default()
            })
            .unwrap();
        if matching {
            matching_ids.push(task.id.clone());
            let source = scratch.join(format!("payload-{index}.bin"));
            std::fs::write(&source, vec![b'a'; ARTIFACT_BYTES]).unwrap();
            runtime
                .run_tool(
                    "orbit.task.artifact.put",
                    json!({
                        "id": task.id, "model": "codex",
                        "path": format!("evidence/payload-{index}.bin"), "source_path": source,
                    }),
                )
                .unwrap();
        }
        progress.advance();
    }
    drop(runtime);

    // Same length, different bytes: a read that hashes the payloads against
    // their manifest refuses every matching bundle.
    progress.phase("rewrite payloads", MATCHING);
    let payloads = find_payloads(root.path());
    assert_eq!(
        payloads.len(),
        MATCHING,
        "one stored payload per matching task"
    );
    for payload in &payloads {
        std::fs::write(payload, vec![b'b'; ARTIFACT_BYTES]).unwrap();
        progress.advance();
    }

    progress.phase("search", 1);
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let response = runtime
        .global_search(GlobalSearchParams {
            query: Some(TERM.into()),
            kind: GlobalSearchKind::Task,
            limit: 10,
            ..Default::default()
        })
        .unwrap();
    progress.advance();
    progress.finish();
    let titles: Vec<_> = response
        .results
        .iter()
        .map(|hit| hit.title.clone().unwrap_or_default())
        .collect();
    assert_eq!(titles.len(), 10, "ten hydrated hits: {titles:?}");
    assert!(
        titles.iter().all(|title| title.contains(TERM)),
        "only matching tasks: {titles:?}"
    );
    // Negative control after the cold search: canonical reads still reject
    // these same bytes. If hydration takes that path, the ten-hit assertion
    // above fails, independently of the host's filesystem or CPU speed.
    let error = runtime.get_task(&matching_ids[0]).unwrap_err();
    assert!(
        matches!(&error, OrbitError::TaskBundleCorrupt { reason, .. }
            if reason.contains("artifact manifest sha256 mismatch")),
        "canonical read must detect the corrupted payload: {error}"
    );
}

/// The stored artifact payloads, wherever and however the bundle layout
/// names them: the only files of exactly [`ARTIFACT_BYTES`].
fn find_payloads(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name != "tmp") {
                    pending.push(path);
                }
            } else if path.metadata().unwrap().len() == ARTIFACT_BYTES as u64 {
                found.push(path);
            }
        }
    }
    found
}

/// Counts the FTS page queries the lexical store executes.
struct Queries(Arc<AtomicU64>);

impl<S: tracing::Subscriber> Layer<S> for Queries {
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        if event.metadata().target() == "orbit.search.fts" {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Observe actual SQLite page executions at the composed-runtime boundary.
/// This guards the fallback's scan cost without replacing the store with a mock.
#[test]
fn full_search_page_skips_any_term_scan() {
    if !super::dispatch_admission::isolated(
        "search_hydration::full_search_page_skips_any_term_scan",
    ) {
        return;
    }
    use tracing_subscriber::prelude::*;

    let root = TempDir::new().unwrap();
    let runtime = OrbitRuntime::from_roots(
        &root.path().join("global"),
        &root.path().join("repo/.orbit"),
    )
    .unwrap();
    for title in [
        "generation participant mcp serve pinned deploy",
        "generation participant",
    ] {
        runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: "Search fixture".into(),
                ..Default::default()
            })
            .unwrap();
    }
    let queries = Arc::new(AtomicU64::new(0));
    let subscriber = tracing_subscriber::registry().with(Queries(Arc::clone(&queries)));
    tracing::subscriber::with_default(subscriber, || {
        let response = runtime
            .global_search(GlobalSearchParams {
                query: Some("generation participant mcp serve pinned deploy".into()),
                kind: GlobalSearchKind::Task,
                limit: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(response.results.len(), 1);
        assert!(response.results[0].matched_by.is_none());
        assert_eq!(
            queries.load(Ordering::Relaxed),
            1,
            "a full page executes exactly one FTS query"
        );

        let response = runtime
            .global_search(GlobalSearchParams {
                query: Some("generation participant mcp serve pinned deploy".into()),
                kind: GlobalSearchKind::Task,
                limit: 2,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(response.results.len(), 2);
        assert_eq!(
            response.results[1].matched_by.as_ref().unwrap(),
            &["partial", "terms:2/6"]
        );
        assert_eq!(
            queries.load(Ordering::Relaxed),
            3,
            "the short page executes both AND and OR queries"
        );
    });
}

/// A status filter that rejects every any-term hit must not make the partial
/// fill page through the whole match set: each FTS page re-sorts all matching
/// chunks, so the query count has to stay constant as the match set grows.
#[test]
fn rejecting_filter_does_not_page_the_any_term_match_set() {
    if !super::dispatch_admission::isolated(
        "search_hydration::rejecting_filter_does_not_page_the_any_term_match_set",
    ) {
        return;
    }
    use tracing_subscriber::prelude::*;

    const COMMON: usize = 60;
    let root = TempDir::new().unwrap();
    let runtime = OrbitRuntime::from_roots(
        &root.path().join("global"),
        &root.path().join("repo/.orbit"),
    )
    .unwrap();
    // No task carries both terms, so the full-match pass finds nothing and
    // every task matches only through the any-term pass.
    for index in 0..COMMON {
        let term = if index % 2 == 0 {
            "alphaterm"
        } else {
            "betaterm"
        };
        runtime
            .add_task(TaskAddParams {
                title: format!("Fixture {index} {term}"),
                description: "Any-term fixture.".into(),
                ..Default::default()
            })
            .unwrap();
    }
    let queries = Arc::new(AtomicU64::new(0));
    let subscriber = tracing_subscriber::registry().with(Queries(Arc::clone(&queries)));
    tracing::subscriber::with_default(subscriber, || {
        let search = |statuses: Vec<String>| {
            runtime
                .global_search(GlobalSearchParams {
                    query: Some("alphaterm betaterm".into()),
                    kind: GlobalSearchKind::Task,
                    status: statuses,
                    limit: 1,
                    ..Default::default()
                })
                .unwrap()
        };
        let rejected = search(vec!["task:done".into()]);
        assert!(rejected.results.is_empty(), "no task is done");
        assert!(
            queries.load(Ordering::Relaxed) <= 2,
            "one AND query and one bounded OR query, not a page per five chunks: {} queries",
            queries.load(Ordering::Relaxed)
        );

        let unfiltered = search(Vec::new());
        assert_eq!(unfiltered.results.len(), 1);
        assert_eq!(
            unfiltered.results[0].matched_by.as_ref().unwrap(),
            &["partial", "terms:1/2"]
        );
    });
}

/// Counts acquisitions of the task store's partition commit lock: the file
/// lock records one event per acquisition a caller paid for, and none for a
/// re-entrant nested section.
struct CommitLockAcquisitions(Arc<AtomicU64>);

impl<S: tracing::Subscriber> Layer<S> for CommitLockAcquisitions {
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        struct LockPath(String);
        impl tracing::field::Visit for LockPath {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "lock_path" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        if event.metadata().target() != "orbit.common.fs.file_lock" {
            return;
        }
        let mut path = LockPath(String::new());
        event.record(&mut path);
        if path.0.ends_with("/.task-commit.lock") {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// A search used to enter the task store's commit boundary once per
/// hydrated hit, each entry taking the partition lock and writing a holder
/// record. Every task read of one search now shares a single acquisition.
#[test]
fn one_search_takes_the_task_commit_lock_once() {
    if !super::dispatch_admission::isolated(
        "search_hydration::one_search_takes_the_task_commit_lock_once",
    ) {
        return;
    }
    use tracing_subscriber::prelude::*;

    let root = tempfile::tempdir_in(test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    for (index, status) in std::iter::repeat_n(orbit_core::TaskStatus::Done, 8)
        .chain(std::iter::repeat_n(orbit_core::TaskStatus::Backlog, 6))
        .enumerate()
    {
        runtime
            .add_task(TaskAddParams {
                title: format!("Fixture {index} {TERM}"),
                description: "Search fixture".into(),
                status: Some(status),
                ..Default::default()
            })
            .unwrap();
    }
    drop(runtime);

    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let acquisitions = Arc::new(AtomicU64::new(0));
    let subscriber =
        tracing_subscriber::registry().with(CommitLockAcquisitions(Arc::clone(&acquisitions)));
    let response = tracing::subscriber::with_default(subscriber, || {
        runtime
            .global_search(GlobalSearchParams {
                query: Some(TERM.into()),
                kind: GlobalSearchKind::Task,
                limit: 10,
                ..Default::default()
            })
            .unwrap()
    });
    assert_eq!(
        response.results.len(),
        6,
        "the default search returns the open matches"
    );
    assert_eq!(
        acquisitions.load(Ordering::Relaxed),
        1,
        "every task read of one search shares one commit-lock acquisition"
    );
}
