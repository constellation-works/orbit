use std::sync::atomic::Ordering;
use std::time::Instant;

use super::*;
use crate::contracts::TaskListFilter;
use crate::driver::file::task_bundle::take_artifact_payload_reads;
use crate::workflow::task::reindex_workspace;

/// Metadata probes charged to the envelope freshness scan since the previous
/// call. Paired with [`reads`], this separates stamping an envelope file from
/// parsing it.
pub(super) fn probes(store: &TaskV2Store) -> usize {
    store.envelope_cache.take_stat_calls()
}

pub(super) fn reads(store: &TaskV2Store) -> (usize, usize) {
    (
        store.bundle_store.bundle_reads.swap(0, Ordering::Relaxed),
        store.bundle_store.envelope_reads.swap(0, Ordering::Relaxed),
    )
}

pub(super) fn corpus(temp: &TempDir, count: usize) -> TaskV2Store {
    let bound = store(temp);
    let store = TaskV2Store::new_checkoutless(bound.registry.clone(), bound.workspace_id.clone());
    for index in 0..count {
        let mut params = create_params(&format!("Task {index}"), TaskStatus::Backlog);
        params.description =
            "A realistic task body with requirements, evidence and implementation details.\n"
                .repeat(100);
        params.plan =
            "Implement the change; verify behavior and document the evidence.\n".repeat(20);
        if index % 10 == 0 {
            params.tags.push("selective".to_string());
        }
        store.create_task(params).expect("create fixture");
    }
    reads(&store);
    store
}

#[test]
fn bounded_queries_load_only_selected_bundles_as_the_corpus_grows() {
    for count in [100, 1000] {
        let temp = TempDir::new().unwrap();
        let store = corpus(&temp, count);
        let page = store
            .query_task_rows(&TaskListFilter::default(), 50, None)
            .unwrap();
        assert_eq!(page.total, count);
        assert_eq!(page.items.len(), 50);
        assert_eq!(reads(&store), (50, count));
        assert!(!page.items[0].comments.is_empty());
        assert!(!page.items[0].history.is_empty());

        let filter = TaskListFilter {
            tags: vec!["selective".to_string()],
            ..Default::default()
        };
        // The freshness scan already parsed every envelope above, so the
        // second query pays metadata probes and no envelope parse.
        let page = store.query_task_rows(&filter, 50, None).unwrap();
        assert_eq!(page.total, count / 10);
        assert_eq!(reads(&store), ((count / 10).min(50), 0));
        assert!(
            page.items
                .iter()
                .all(|row| row.task.tags.contains(&"selective".to_string()))
        );
    }
}

#[test]
fn bounded_integrity_is_selected_only_but_direct_unbounded_and_fallback_reads_are_strict() {
    for corruption in ["body", "events"] {
        let temp = TempDir::new().unwrap();
        let store = store(&temp);
        let old = store
            .create_task(create_params("Old matching", TaskStatus::Backlog))
            .unwrap();
        store
            .upsert_task_artifacts(
                &old.id,
                &TaskArtifactUpdateParams {
                    owner_run_id: None,
                    actor: "codex".to_string(),
                    upsert_artifacts: vec![TaskArtifact {
                        path: "proof.txt".to_string(),
                        media_type: "text/plain".to_string(),
                        content: b"proof".to_vec(),
                        created_by: None,
                    }],
                },
            )
            .unwrap();
        let newest = store
            .create_task(create_params("New task", TaskStatus::Backlog))
            .unwrap();
        let path = store.bundle_store.bundle_path(&old.id).unwrap();
        match corruption {
            "body" => fs::remove_file(path.join("description.md")).unwrap(),
            "events" => {
                let event = TaskEventRowV2 {
                    schema_version: 1,
                    event_id: "EV-0001".to_string(),
                    at: Utc::now(),
                    by: "codex".to_string(),
                    event_type: "status_changed".to_string(),
                    note: None,
                    from_status: Some(TaskStatus::Backlog),
                    to_status: Some(TaskStatus::Done),
                };
                fs::write(
                    path.join("events.jsonl"),
                    format!("{}\n", serde_json::to_string(&event).unwrap()),
                )
                .unwrap();
            }
            _ => fs::write(path.join("artifacts/files/proof.txt"), "wrong").unwrap(),
        }
        let page = store
            .query_task_rows(&TaskListFilter::default(), 1, None)
            .unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.items[0].task.id, newest.id);
        assert!(
            store
                .query_task_rows(&TaskListFilter::default(), 2, None)
                .is_err(),
            "{corruption}"
        );
        assert!(store.get_task_row(&old.id, false).is_err());
        assert!(store.list_tasks().is_err());
        assert!(
            store
                .query_task_rows(&TaskListFilter::default(), 1, Some(&|_, _| true))
                .is_err()
        );
        let conn = rusqlite::Connection::open(task_registry_path(temp.path())).unwrap();
        conn.execute("DELETE FROM task_bundle_index", []).unwrap();
        assert!(
            store
                .query_task_rows(&TaskListFilter::default(), 1, None)
                .is_err()
        );
    }
}

#[test]
fn missing_and_stale_indexes_rebuild_then_return_to_bounded_reads() {
    let temp = TempDir::new().unwrap();
    let store = corpus(&temp, 10);
    // Parse every envelope once up front, so each iteration below measures the
    // rebuild itself rather than the first freshness scan's parses.
    store
        .query_task_rows(&TaskListFilter::default(), 2, None)
        .unwrap();
    reads(&store);
    let conn = rusqlite::Connection::open(task_registry_path(temp.path())).unwrap();
    for sql in [
        "DELETE FROM task_bundle_index",
        "UPDATE task_bundle_index SET updated_at = 'stale'",
    ] {
        conn.execute(sql, []).unwrap();
        let page = store
            .query_task_rows(&TaskListFilter::default(), 2, None)
            .unwrap();
        assert_eq!(page.total, 10);
        assert_eq!(page.items.len(), 2);
        assert_eq!(reads(&store).0, 12);
        store
            .query_task_rows(&TaskListFilter::default(), 2, None)
            .unwrap();
        assert_eq!(reads(&store), (2, 0));
    }
}

#[test]
fn residual_filter_runs_before_limit_and_does_not_lose_older_matches() {
    let temp = TempDir::new().unwrap();
    let store = corpus(&temp, 60);
    let page = store
        .query_task_rows(
            &TaskListFilter::default(),
            1,
            Some(&|task, _| task.title == "Task 0"),
        )
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].task.title, "Task 0");
    assert_eq!(reads(&store), (60, 60));
}

#[test]
fn selected_corruption_is_not_replaced_and_in_flight_deletion_is_tolerated() {
    let temp = TempDir::new().unwrap();
    let store = corpus(&temp, 3);
    let selected = store
        .task_candidates(&TaskListFilter::default(), 1)
        .unwrap()
        .items
        .remove(0);
    let path = store.bundle_store.bundle_path(&selected.id).unwrap();
    fs::remove_file(path.join("description.md")).unwrap();
    assert!(
        store
            .query_task_rows(&TaskListFilter::default(), 1, None)
            .is_err()
    );
    let sentinel = path.with_file_name(format!(".{}.lock", selected.id));
    fs::write(sentinel, "").unwrap();
    assert!(
        store
            .query_task_rows(&TaskListFilter::default(), 1, None)
            .is_err()
    );
    fs::remove_dir_all(path).unwrap();
    assert_eq!(
        store
            .query_task_rows(&TaskListFilter::default(), 3, None)
            .unwrap()
            .items
            .len(),
        2
    );
}

#[test]
fn an_update_between_selection_and_hydration_rechecks_filters_once() {
    let temp = TempDir::new().unwrap();
    let store = corpus(&temp, 3);
    let old = store
        .task_candidates(&TaskListFilter::default(), 3)
        .unwrap()
        .items
        .pop()
        .unwrap();
    let updated = std::sync::atomic::AtomicBool::new(false);
    let residual = |task: &Task, _: &BTreeMap<String, TaskStatus>| {
        if !updated.swap(true, Ordering::Relaxed) {
            store
                .update_task_document(
                    &old.id,
                    &TaskDocumentUpdateParams {
                        actor: "codex".to_string(),
                        title: Some("Changed during query".to_string()),
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        task.title == "Changed during query"
    };
    let page = store
        .query_task_rows(&TaskListFilter::default(), 1, Some(&residual))
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].task.id, old.id);
    assert_eq!(page.items[0].task.title, "Changed during query");
    // Two unchanged rows, one changed row, then a single three-bundle scan.
    assert_eq!(reads(&store).0, 7); // includes the update's read of its bundle
}

#[test]
fn metadata_filters_preserve_ties_and_legacy_tag_normalization() {
    let temp = TempDir::new().unwrap();
    let store = corpus(&temp, 3);
    let candidates = store
        .task_candidates(&TaskListFilter::default(), 3)
        .unwrap();
    let tied_at = candidates.items[0].created_at;
    for mut envelope in candidates.items {
        envelope.created_at = tied_at;
        envelope.tags = vec![" Mixed-Case ".to_string()];
        store
            .bundle_store
            .rewrite_envelope(&envelope.id, &envelope)
            .unwrap();
        store
            .registry
            .replace_task_index(&store.workspace_id, &envelope)
            .unwrap();
    }
    let filter = TaskListFilter {
        tags: vec![" MIXED-case ".to_string()],
        statuses: Some(vec![TaskStatus::Backlog]),
        priority: Some(TaskPriority::High),
        task_type: Some(TaskType::Feature),
        external_ref: Some(
            ExternalRef::try_new("linear".to_string(), "ENG-123".to_string(), None).unwrap(),
        ),
        has_external_ref_system: Some("linear".to_string()),
        ..Default::default()
    };
    let expected = store
        .list_tasks()
        .unwrap()
        .into_iter()
        .map(|task| task.id)
        .collect::<Vec<_>>();
    let page = store.query_task_rows(&filter, 2, None).unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(
        page.items
            .iter()
            .map(|row| &row.task.id)
            .collect::<Vec<_>>(),
        expected[..2].iter().collect::<Vec<_>>()
    );
}

fn upsert_proof(store: &TaskV2Store, id: &str, content: &[u8]) {
    store
        .upsert_task_artifacts(
            id,
            &TaskArtifactUpdateParams {
                owner_run_id: None,
                actor: "codex".to_string(),
                upsert_artifacts: vec![TaskArtifact {
                    path: "proof.txt".to_string(),
                    media_type: "text/plain".to_string(),
                    content: content.to_vec(),
                    created_by: None,
                }],
            },
        )
        .unwrap();
}

#[test]
fn listing_and_search_defer_artifact_payload_verification() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let old = store
        .create_task(create_params("Old matching", TaskStatus::Backlog))
        .unwrap();
    upsert_proof(&store, &old.id, b"proof");
    let newest = store
        .create_task(create_params("New task", TaskStatus::Backlog))
        .unwrap();
    let path = store.bundle_store.bundle_path(&old.id).unwrap();
    fs::write(path.join("artifacts/files/proof.txt"), "wrong").unwrap();
    let _ = take_artifact_payload_reads();

    let listed = store.list_tasks().unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>(),
        vec![newest.id.as_str(), old.id.as_str()]
    );
    let searched = store.search_tasks("old matching").unwrap();
    assert_eq!(
        searched
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>(),
        vec![old.id.as_str()]
    );
    let page = store
        .query_task_rows(&TaskListFilter::default(), 2, None)
        .unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.items[0].task.id, newest.id);
    assert_eq!(page.items[1].task.id, old.id);
    assert_eq!(page.items[1].artifacts[0].path, "proof.txt");
    assert_eq!(take_artifact_payload_reads(), 0);

    assert!(store.get_task(&old.id).is_err());
    assert!(store.get_task_row(&old.id, false).is_err());
    assert!(store.get_task_artifact(&old.id, "proof.txt").is_err());
    assert!(reindex_workspace(&store.registry, &store.workspace_id).is_err());
    let _ = take_artifact_payload_reads();

    let conn = rusqlite::Connection::open(task_registry_path(temp.path())).unwrap();
    conn.execute("DELETE FROM task_bundle_index", []).unwrap();
    let rebuilt = store
        .query_task_rows(&TaskListFilter::default(), 2, None)
        .unwrap();
    assert_eq!(rebuilt.total, 2);
    assert_eq!(take_artifact_payload_reads(), 0);
}

#[test]
#[allow(clippy::print_stdout)]
fn lightweight_listing_skips_artifact_payload_io() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    const TASKS: usize = 8;
    const BLOB_BYTES: usize = 512 * 1024;
    let payload = vec![b'x'; BLOB_BYTES];
    let mut ids = Vec::with_capacity(TASKS);
    for index in 0..TASKS {
        let task = store
            .create_task(create_params(
                &format!("Heavy artifact task {index}"),
                TaskStatus::Backlog,
            ))
            .unwrap();
        upsert_proof(&store, &task.id, &payload);
        ids.push(task.id);
    }
    let _ = take_artifact_payload_reads();
    let _ = store.list_tasks().unwrap();
    let _ = store.search_tasks("heavy artifact").unwrap();
    let _ = store
        .query_task_rows(&TaskListFilter::default(), TASKS, None)
        .unwrap();
    for id in &ids {
        let _ = store.get_task(id).unwrap();
    }
    let _ = take_artifact_payload_reads();

    let list_started = Instant::now();
    let listed = store.list_tasks().unwrap();
    let list_ms = list_started.elapsed().as_secs_f64() * 1000.0;
    let list_payload_opens = take_artifact_payload_reads();

    let search_started = Instant::now();
    let searched = store.search_tasks("heavy artifact").unwrap();
    let search_ms = search_started.elapsed().as_secs_f64() * 1000.0;
    let search_payload_opens = take_artifact_payload_reads();

    let rows_started = Instant::now();
    let page = store
        .query_task_rows(&TaskListFilter::default(), TASKS, None)
        .unwrap();
    let rows_ms = rows_started.elapsed().as_secs_f64() * 1000.0;
    let row_payload_opens = take_artifact_payload_reads();

    let strict_started = Instant::now();
    for id in &ids {
        store.get_task(id).unwrap().unwrap();
    }
    let strict_ms = strict_started.elapsed().as_secs_f64() * 1000.0;
    let strict_payload_opens = take_artifact_payload_reads();

    let manifest_started = Instant::now();
    for id in &ids {
        let manifest = store.get_task_artifact_manifest(id).unwrap().unwrap();
        assert_eq!(manifest.len(), 1);
    }
    let manifest_ms = manifest_started.elapsed().as_secs_f64() * 1000.0;
    let manifest_payload_opens = take_artifact_payload_reads();

    let single_artifact = store
        .get_task_artifact(&ids[0], "proof.txt")
        .unwrap()
        .unwrap();
    assert_eq!(single_artifact.content.len(), BLOB_BYTES);
    let single_artifact_opens = take_artifact_payload_reads();

    assert_eq!(listed.len(), TASKS);
    assert_eq!(searched.len(), TASKS);
    assert_eq!(page.items.len(), TASKS);
    assert_eq!(list_payload_opens, 0);
    assert_eq!(search_payload_opens, 0);
    assert_eq!(row_payload_opens, 0);
    assert_eq!(manifest_payload_opens, 0);
    assert_eq!(single_artifact_opens, 1);
    assert_eq!(strict_payload_opens, TASKS);

    println!(
        "{}",
        serde_json::json!({
            "tasks": TASKS,
            "blob_bytes_per_task": BLOB_BYTES,
            "lightweight_list_ms": list_ms,
            "lightweight_search_ms": search_ms,
            "lightweight_rows_ms": rows_ms,
            "lightweight_manifest_ms": manifest_ms,
            "strict_get_all_ms": strict_ms,
            "lightweight_list_payload_opens": list_payload_opens,
            "lightweight_search_payload_opens": search_payload_opens,
            "lightweight_rows_payload_opens": row_payload_opens,
            "strict_get_all_payload_opens": strict_payload_opens,
        })
    );
}
