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
    let response = runtime
        .global_search(GlobalSearchParams {
            query: Some(TERM.into()),
            kind: GlobalSearchKind::Task,
            limit: 10,
            ..Default::default()
        })
        .unwrap();
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
        elapsed < Duration::from_millis(300),
        "cold search over {TASKS} tasks took {elapsed:?}"
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
