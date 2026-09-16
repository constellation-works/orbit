use std::sync::atomic::Ordering;
use std::time::Instant;

use orbit_types::task::task_dependencies_ready;

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
    let store = TaskV2Store::new(bound.registry.clone(), bound.workspace_id.clone());
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
fn title_search_filters_metadata_before_bounded_hydration() {
    let temp = TempDir::new().unwrap();
    let store = corpus(&temp, 120);
    let page = store
        .query_task_rows(
            &TaskListFilter {
                search: Some("task 1".to_string()),
                ..Default::default()
            },
            7,
            None,
        )
        .unwrap();

    assert_eq!(page.total, 31);
    assert_eq!(page.items.len(), 7);
    assert!(
        page.items
            .iter()
            .all(|row| row.task.title.to_lowercase().contains("task 1"))
    );
    assert_eq!(reads(&store).0, 7, "only the selected page is hydrated");
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
    assert_eq!(page.total_without_cursor, 3);
    assert_eq!(
        page.items
            .iter()
            .map(|row| &row.task.id)
            .collect::<Vec<_>>(),
        expected[..2].iter().collect::<Vec<_>>()
    );
    let boundary = page.items.last().unwrap();
    let continuation = store
        .query_task_rows(
            &TaskListFilter {
                scan_before: Some((boundary.task.created_at, boundary.task.id.clone())),
                ..filter
            },
            2,
            None,
        )
        .unwrap();
    assert_eq!(continuation.items.len(), 1);
    assert_eq!(continuation.items[0].task.id, expected[2]);
    assert_eq!(continuation.total, 1);
    assert_eq!(continuation.total_without_cursor, 3);
}

#[test]
fn cursor_page_keeps_unbounded_total_on_the_indexed_path() {
    let temp = TempDir::new().unwrap();
    let store = corpus(&temp, 3);
    let page = store
        .query_task_rows(&TaskListFilter::default(), 2, None)
        .unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(page.total_without_cursor, 3);
    assert_eq!(page.items.len(), 2);
    let boundary = page.items.last().unwrap();
    let continuation = store
        .query_task_rows(
            &TaskListFilter {
                scan_before: Some((boundary.task.created_at, boundary.task.id.clone())),
                ..Default::default()
            },
            2,
            None,
        )
        .unwrap();
    assert_eq!(continuation.items.len(), 1);
    assert_eq!(continuation.total, 1);
    assert_eq!(continuation.total_without_cursor, 3);
    let candidates = store
        .task_candidates(
            &TaskListFilter {
                scan_before: Some((boundary.task.created_at, boundary.task.id.clone())),
                ..Default::default()
            },
            2,
        )
        .unwrap();
    assert_eq!(candidates.items.len(), 1);
    assert_eq!(candidates.total, 1);
    assert_eq!(candidates.total_without_cursor, 3);
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

/// A second store on the same registry, bound as its own workspace.
fn sibling_store(temp: &TempDir, store: &TaskV2Store, partition_id: &str) -> TaskV2Store {
    let repo_dir = temp.path().join(partition_id);
    let orbit_dir = repo_dir.join(".orbit");
    fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    let binding = store
        .registry
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some(partition_id.to_string()),
            slug: partition_id.to_string(),
            repo_root: repo_dir.clone(),
            workspace_path: repo_dir,
            orbit_dir,
            repo_fingerprint: None,
        })
        .expect("bind sibling workspace");
    TaskV2Store::new(store.registry.clone(), binding.partition_id)
}

#[test]
fn status_aware_listing_partitions_one_bounded_scan() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut ids = Vec::new();
    for (index, status) in [
        TaskStatus::Done,
        TaskStatus::Backlog,
        TaskStatus::Archived,
        TaskStatus::Review,
        TaskStatus::Rejected,
        TaskStatus::Backlog,
    ]
    .into_iter()
    .enumerate()
    {
        let task = store
            .create_task(create_params(&format!("Task {index}"), status))
            .unwrap();
        ids.push(task.id);
    }
    reads(&store);

    let terminal_last = TaskListFilter {
        terminal_last: true,
        ..Default::default()
    };
    let page = store.query_task_rows(&terminal_last, 4, None).unwrap();
    assert_eq!(page.total, 6, "the total spans both partitions");
    assert_eq!(
        page.items
            .iter()
            .map(|row| row.task.id.as_str())
            .collect::<Vec<_>>(),
        vec![
            ids[5].as_str(),
            ids[3].as_str(),
            ids[1].as_str(),
            ids[4].as_str()
        ],
        "non-terminal newest first, then terminal newest first, within one limit"
    );
    assert_eq!(reads(&store).0, 4, "only the page is hydrated");

    // The residual path hydrates every candidate but keeps the partition.
    let page = store
        .query_task_rows(&terminal_last, 2, Some(&|task, _| task.title != "Task 3"))
        .unwrap();
    assert_eq!(page.total, 5);
    assert_eq!(
        page.items
            .iter()
            .map(|row| row.task.id.as_str())
            .collect::<Vec<_>>(),
        vec![ids[5].as_str(), ids[1].as_str()]
    );
}

#[test]
fn status_projection_is_scoped_to_the_workspace_and_the_targets_the_page_names() {
    let temp = TempDir::new().unwrap();
    let alpha = store(&temp);
    let beta = sibling_store(&temp, &alpha, "beta-bbbbbb");
    let target = beta
        .create_task(create_params("Cross-workspace target", TaskStatus::Done))
        .unwrap();
    let unrelated = beta
        .create_task(create_params("Unrelated in beta", TaskStatus::Backlog))
        .unwrap();
    let mut dependent = create_params("Depends across workspaces", TaskStatus::Backlog);
    dependent.dependencies = vec![target.id.clone()];
    let dependent = alpha.create_task(dependent).unwrap();
    let local = alpha
        .create_task(create_params("Local only", TaskStatus::Review))
        .unwrap();

    let page = alpha
        .query_task_rows(&TaskListFilter::default(), 50, None)
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.status_by_id.get(&target.id), Some(&TaskStatus::Done));
    assert_eq!(
        page.status_by_id.get(&dependent.id),
        Some(&TaskStatus::Backlog)
    );
    assert_eq!(page.status_by_id.get(&local.id), Some(&TaskStatus::Review));
    assert!(
        !page.status_by_id.contains_key(&unrelated.id),
        "other workspaces contribute only the targets the page references"
    );
    assert!(
        task_dependencies_ready(&page.items[0].task, &page.status_by_id)
            && task_dependencies_ready(&page.items[1].task, &page.status_by_id)
    );

    // Readiness over the residual path resolves the same cross-workspace target.
    let ready = alpha
        .query_task_rows(
            &TaskListFilter::default(),
            50,
            Some(&|task, statuses| task_dependencies_ready(task, statuses)),
        )
        .unwrap();
    assert_eq!(ready.total, 2);

    // The whole workspace is projected even when the page is narrower than it.
    let page = alpha
        .query_task_rows(&TaskListFilter::default(), 1, None)
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(page.status_by_id.contains_key(&dependent.id));
    assert!(page.status_by_id.contains_key(&local.id));
}

/// A hand edit that keeps `updated_at` but changes a filter field is seen by
/// a process that never parsed the old envelope: the index row disagrees
/// with the envelope on that field, so the scan rebuilds before selecting.
#[test]
fn a_cold_scan_rebuilds_the_index_for_a_filter_field_edited_in_place() {
    let temp = TempDir::new().unwrap();
    let warm = corpus(&temp, 4);
    let mut envelope = warm
        .task_candidates(&TaskListFilter::default(), 1)
        .unwrap()
        .items
        .remove(0);
    let path = warm.bundle_store.envelope_path(&envelope.id).unwrap();
    envelope.tags = vec!["hand-edited".to_string()];
    envelope.priority = TaskPriority::Low;
    fs::write(&path, serde_yaml::to_string(&envelope).unwrap()).unwrap();

    let cold = TaskV2Store::new(warm.registry.clone(), warm.workspace_id.clone());
    let hand_edited = TaskListFilter {
        tags: vec!["hand-edited".to_string()],
        priority: Some(TaskPriority::Low),
        ..Default::default()
    };
    let candidates = cold.task_candidates(&hand_edited, 4).unwrap();
    assert_eq!(candidates.total, 1);
    assert_eq!(candidates.items[0].id, envelope.id);
    assert_eq!(
        cold.registry
            .indexed_task_ids_filtered(&cold.workspace_id, &hand_edited.index_filter(Vec::new()))
            .unwrap(),
        vec![envelope.id.clone()],
        "the rebuilt index projects the edited fields"
    );
    reads(&cold);
    probes(&cold);
    let candidates = cold.task_candidates(&hand_edited, 4).unwrap();
    assert_eq!(candidates.items[0].id, envelope.id);
    assert_eq!((probes(&cold), reads(&cold)), (4, (0, 0)));
}
