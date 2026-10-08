//! Lexical search hydrates its hits from the task documents alone: a cold
//! search over a large workspace whose matching bundles carry megabytes of
//! artifacts neither reads nor verifies those payloads.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use orbit_core::application::task::TaskAddParams;
use orbit_core::{GlobalSearchKind, GlobalSearchParams, OrbitRuntime};
use serde_json::json;
use tempfile::TempDir;

/// Tasks in the workspace; only [`MATCHING`] of them carry the query term.
const TASKS: usize = 4_000;
/// Matching tasks, each carrying one [`ARTIFACT_BYTES`] artifact: 20 MB.
const MATCHING: usize = 20;
const ARTIFACT_BYTES: usize = 1_000_000;
const TERM: &str = "quasarbeacon";

#[test]
fn cold_search_hits_skip_artifact_payloads() {
    if !super::dispatch_admission::isolated(
        "search_hydration::cold_search_hits_skip_artifact_payloads",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();

    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let scratch = workspace.join("tmp");
    std::fs::create_dir_all(&scratch).unwrap();
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
    }
    drop(runtime);

    // Same length, different bytes: a read that hashes the payloads against
    // their manifest refuses every matching bundle.
    let payloads = find_payloads(root.path());
    assert_eq!(
        payloads.len(),
        MATCHING,
        "one stored payload per matching task"
    );
    for payload in &payloads {
        std::fs::write(payload, vec![b'b'; ARTIFACT_BYTES]).unwrap();
    }

    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let started = Instant::now();
    let work_before = work_clock();
    let response = runtime
        .global_search(GlobalSearchParams {
            query: Some(TERM.into()),
            kind: GlobalSearchKind::Task,
            limit: 10,
            ..Default::default()
        })
        .unwrap();
    let work = work_clock().saturating_sub(work_before);
    let elapsed = started.elapsed();
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
    assert!(
        work < Duration::from_millis(300),
        "cold search over {TASKS} tasks used {work:?} of CPU ({elapsed:?} wall, {})",
        orbit_common::test_env::host_load()
    );
}

/// The clock the search's cost is bounded on. Reading and hashing 20 MB of
/// payloads is work, so on Unix this is the process's user and system CPU
/// time: a saturated host stretches wall-clock time without changing the work
/// done. Elsewhere it is wall-clock time since first use.
#[cfg(unix)]
fn work_clock() -> Duration {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // Safety: `getrusage` fills the `rusage` it is handed and reads nothing else.
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    // Safety: a zero return means the kernel filled `usage`.
    let usage = unsafe { usage.assume_init() };
    let time = |value: libc::timeval| {
        Duration::from_secs(value.tv_sec as u64) + Duration::from_micros(value.tv_usec as u64)
    };
    time(usage.ru_utime) + time(usage.ru_stime)
}

#[cfg(not(unix))]
fn work_clock() -> Duration {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
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

/// Observe actual SQLite page executions at the composed-runtime boundary.
/// This guards the fallback's scan cost without replacing the store with a mock.
#[test]
fn full_search_page_skips_any_term_scan() {
    if !super::dispatch_admission::isolated(
        "search_hydration::full_search_page_skips_any_term_scan",
    ) {
        return;
    }
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    use tracing_subscriber::{Layer, layer::Context, prelude::*};

    struct Queries(Arc<AtomicU64>);
    impl<S: tracing::Subscriber> Layer<S> for Queries {
        fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
            if event.metadata().target() == "orbit.search.fts" {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
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
