//! ORB-11648: indexed selection reuses parsed envelopes for unchanged files.
//!
//! Every test here pins one half of the freshness policy documented on
//! [`super::super::envelope_cache`]: a warm scan must parse nothing, and any
//! change on disk — a store write, an out-of-band rewrite, a deletion, or a
//! corrupt file — must still be observed exactly as it was before the cache.

use std::sync::atomic::Ordering;
use std::time::Instant;

use super::listing::{corpus, probes, reads};
use super::listing_bench::seed;
use super::*;
use crate::contracts::{TaskCandidates, TaskListFilter};
use crate::workflow::task::reindex_workspace;

const CORPUS: usize = 200;

fn filters() -> Vec<TaskListFilter> {
    vec![
        TaskListFilter::default(),
        TaskListFilter {
            tags: vec!["selective".to_string()],
            ..Default::default()
        },
        TaskListFilter {
            statuses: Some(vec![TaskStatus::Backlog]),
            ..Default::default()
        },
        TaskListFilter {
            statuses: Some(vec![TaskStatus::Review]),
            ..Default::default()
        },
        TaskListFilter {
            priority: Some(TaskPriority::High),
            ..Default::default()
        },
    ]
}

/// Selection order, filtering and totals recomputed from a strict scan of the
/// bundles on disk, with neither the generated index nor the cache in the path.
fn strict_reference(
    store: &TaskV2Store,
    filter: &TaskListFilter,
    limit: usize,
) -> (Vec<String>, usize) {
    let filter = filter.normalized();
    let mut envelopes = store
        .bundle_store
        .list_bundles()
        .expect("strict bundle scan")
        .into_iter()
        .map(|bundle| bundle.envelope)
        .filter(|envelope| filter.matches(envelope))
        .collect::<Vec<_>>();
    sort_by_created_desc_id_asc(
        &mut envelopes,
        |envelope| &envelope.created_at,
        |envelope| &envelope.id,
    );
    let total = envelopes.len();
    let mut ids = envelopes
        .into_iter()
        .map(|envelope| envelope.id)
        .collect::<Vec<_>>();
    ids.truncate(limit);
    (ids, total)
}

fn selected_ids(candidates: &TaskCandidates) -> Vec<String> {
    candidates
        .items
        .iter()
        .map(|envelope| envelope.id.clone())
        .collect()
}

/// Metadata probes and envelope parses charged since the previous call.
fn scan_cost(store: &TaskV2Store) -> (usize, usize) {
    (probes(store), reads(store).1)
}

#[test]
fn a_warm_candidate_selection_parses_no_envelopes_and_matches_the_strict_reference() {
    let temp = TempDir::new().expect("tempdir");
    let store = corpus(&temp, CORPUS);
    let filters = filters();
    let expected = filters
        .iter()
        .map(|filter| strict_reference(&store, filter, 50))
        .collect::<Vec<_>>();
    scan_cost(&store);

    // Cold: one metadata probe and one envelope parse per registered task.
    store
        .task_candidates(&filters[0], 50)
        .expect("cold selection");
    assert_eq!(scan_cost(&store), (CORPUS, CORPUS));

    // Warm: the same metadata work, and no envelope parse at all.
    for (filter, (expected_ids, expected_total)) in filters.iter().zip(expected) {
        let candidates = store.task_candidates(filter, 50).expect("warm selection");
        assert_eq!(
            scan_cost(&store),
            (CORPUS, 0),
            "a warm scan must stat every task and parse none"
        );
        assert_eq!(candidates.total, expected_total);
        assert_eq!(selected_ids(&candidates), expected_ids);
    }
}

#[test]
fn a_status_transition_reparses_only_the_task_it_rewrote() {
    let temp = TempDir::new().expect("tempdir");
    let store = corpus(&temp, 8);
    store
        .task_candidates(&TaskListFilter::default(), 8)
        .expect("warm the cache");
    let moved = store
        .task_candidates(&TaskListFilter::default(), 1)
        .expect("select one")
        .items
        .remove(0);
    scan_cost(&store);

    store
        .update_task_history(
            &moved.id,
            &TaskHistoryUpdateParams {
                actor: "codex".to_string(),
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .expect("transition");
    reads(&store);

    let review = TaskListFilter {
        statuses: Some(vec![TaskStatus::Review]),
        ..Default::default()
    };
    let candidates = store.task_candidates(&review, 8).expect("selection");
    assert_eq!(
        scan_cost(&store),
        (8, 1),
        "only the rewritten envelope reparses"
    );
    assert_eq!(selected_ids(&candidates), vec![moved.id.clone()]);
    // The transition kept the generated index in step, so the indexed status
    // filter agrees with the freshly parsed envelope.
    assert_eq!(
        store
            .list_tasks_filtered(Some(TaskStatus::Review), None, None, None, None, None)
            .expect("indexed status filter")
            .into_iter()
            .map(|task| task.id)
            .collect::<Vec<_>>(),
        vec![moved.id.clone()]
    );

    scan_cost(&store);
    let candidates = store.task_candidates(&review, 8).expect("selection");
    assert_eq!(scan_cost(&store), (8, 0), "the new parse is reused in turn");
    assert_eq!(selected_ids(&candidates), vec![moved.id]);
}

#[test]
fn an_out_of_band_atomic_replacement_invalidates_the_cache_and_the_index() {
    let temp = TempDir::new().expect("tempdir");
    let store = corpus(&temp, 4);
    let mut envelope = store
        .task_candidates(&TaskListFilter::default(), 4)
        .expect("warm the cache")
        .items
        .remove(0);
    scan_cost(&store);

    // Republished exactly as the store publishes an envelope — staged, then
    // renamed over the target — but without the matching index write.
    envelope.priority = TaskPriority::Low;
    envelope.tags = vec!["re-tagged".to_string()];
    envelope.updated_at += chrono::Duration::seconds(1);
    store
        .bundle_store
        .rewrite_envelope(&envelope.id, &envelope)
        .expect("republish envelope");

    let low = TaskListFilter {
        priority: Some(TaskPriority::Low),
        tags: vec!["re-tagged".to_string()],
        ..Default::default()
    };
    let (expected_ids, expected_total) = strict_reference(&store, &low, 4);
    reads(&store);
    let candidates = store.task_candidates(&low, 4).expect("selection");
    assert_eq!(selected_ids(&candidates), expected_ids);
    assert_eq!(candidates.total, expected_total);
    assert_eq!(expected_ids, vec![envelope.id.clone()]);

    // `updated_at` no longer matches the index row, so the whole index is
    // rebuilt from the bundles and the indexed filters refresh with it.
    assert_eq!(
        store
            .list_tasks_by_tags(&["re-tagged".to_string()])
            .expect("indexed tag filter")
            .into_iter()
            .map(|task| task.id)
            .collect::<Vec<_>>(),
        vec![envelope.id.clone()]
    );

    scan_cost(&store);
    let candidates = store.task_candidates(&low, 4).expect("selection");
    assert_eq!(
        scan_cost(&store),
        (4, 0),
        "the republished envelope is cached in turn"
    );
    assert_eq!(selected_ids(&candidates), vec![envelope.id]);
}

/// An editor that truncates and rewrites `task.yaml` keeps the file's identity,
/// so only its length and timestamp witness the change. The scan must still
/// re-read it, even though the unchanged `updated_at` leaves the index valid.
#[cfg(unix)]
#[test]
fn an_in_place_rewrite_that_keeps_updated_at_still_refreshes_filter_fields() {
    use std::os::unix::fs::MetadataExt;

    let temp = TempDir::new().expect("tempdir");
    let store = corpus(&temp, 4);
    let mut envelope = store
        .task_candidates(&TaskListFilter::default(), 4)
        .expect("warm the cache")
        .items
        .remove(0);
    let path = store
        .bundle_store
        .envelope_path(&envelope.id)
        .expect("envelope path");
    let identity_before = fs::metadata(&path).expect("metadata").ino();

    envelope.title = "Edited outside the store by hand".to_string();
    envelope.tags = vec!["hand-edited".to_string()];
    fs::write(
        &path,
        serde_yaml::to_string(&envelope).expect("serialize envelope"),
    )
    .expect("in-place rewrite");
    assert_eq!(
        fs::metadata(&path).expect("metadata").ino(),
        identity_before,
        "the rewrite must reuse the file to exercise the length/timestamp stamp"
    );

    let hand_edited = TaskListFilter {
        tags: vec!["hand-edited".to_string()],
        ..Default::default()
    };
    let (expected_ids, expected_total) = strict_reference(&store, &hand_edited, 4);
    scan_cost(&store);
    let candidates = store.task_candidates(&hand_edited, 4).expect("selection");
    assert_eq!(
        scan_cost(&store),
        (4, 1),
        "only the edited envelope reparses"
    );
    assert_eq!(candidates.total, expected_total);
    assert_eq!(selected_ids(&candidates), expected_ids);
    assert_eq!(
        candidates.items[0].title,
        "Edited outside the store by hand"
    );
}

#[test]
fn deleted_and_in_flight_bundles_drop_their_cached_parses() {
    let temp = TempDir::new().expect("tempdir");
    let store = corpus(&temp, 4);
    let selected = store
        .task_candidates(&TaskListFilter::default(), 4)
        .expect("warm the cache")
        .items;
    assert_eq!(store.envelope_cache.entry_count(), 4);

    assert!(store.delete_task(&selected[0].id).expect("delete task"));
    let candidates = store
        .task_candidates(&TaskListFilter::default(), 4)
        .expect("selection after deletion");
    assert_eq!(candidates.total, 3);
    assert!(!selected_ids(&candidates).contains(&selected[0].id));
    assert_eq!(
        store.envelope_cache.entry_count(),
        3,
        "an unregistered task must not keep a cached parse"
    );

    // A bundle removed under a live binding is a concurrent deletion the scan
    // skips; its cached parse must go with it rather than resurrect the task.
    let path = store
        .bundle_store
        .bundle_path(&selected[1].id)
        .expect("bundle path");
    fs::remove_dir_all(&path).expect("remove bundle");
    let candidates = store
        .task_candidates(&TaskListFilter::default(), 4)
        .expect("selection with an in-flight bundle");
    assert_eq!(candidates.total, 2);
    assert!(!selected_ids(&candidates).contains(&selected[1].id));
    assert_eq!(store.envelope_cache.entry_count(), 2);
}

#[test]
fn missing_and_legacy_index_metadata_fall_back_to_the_strict_scan() {
    let temp = TempDir::new().expect("tempdir");
    let store = corpus(&temp, 6);
    let (expected_ids, expected_total) = strict_reference(&store, &TaskListFilter::default(), 6);
    store
        .task_candidates(&TaskListFilter::default(), 6)
        .expect("warm the cache");

    let conn = rusqlite::Connection::open(task_registry_path(temp.path())).expect("open registry");
    for sql in [
        "DELETE FROM task_bundle_index",
        "UPDATE task_bundle_index SET updated_at = 'stale'",
        "UPDATE task_bundle_index SET complexity = NULL",
    ] {
        conn.execute(sql, []).expect("degrade the index");
        let candidates = store
            .task_candidates(&TaskListFilter::default(), 6)
            .expect("selection over a degraded index");
        assert_eq!(candidates.total, expected_total, "{sql}");
        assert_eq!(selected_ids(&candidates), expected_ids, "{sql}");
        assert_eq!(
            store.task_complexity_by_id().expect("complexity").len(),
            expected_total,
            "{sql}"
        );
    }
}

#[test]
fn a_corrupt_envelope_is_reported_by_selection_and_by_explicit_reindex() {
    let temp = TempDir::new().expect("tempdir");
    let store = corpus(&temp, 3);
    let selected = store
        .task_candidates(&TaskListFilter::default(), 3)
        .expect("warm the cache")
        .items;
    assert!(reindex_workspace(&store.registry, &store.workspace_id).is_ok());

    let path = store
        .bundle_store
        .envelope_path(&selected[0].id)
        .expect("envelope path");
    fs::write(&path, "not: [a valid, envelope").expect("corrupt the envelope");

    let error = store
        .task_candidates(&TaskListFilter::default(), 3)
        .expect_err("a cached parse must not hide a corrupt envelope");
    assert!(
        error.to_string().contains(&selected[0].id),
        "expected the corrupt task id in `{error}`"
    );
    let error = reindex_workspace(&store.registry, &store.workspace_id)
        .expect_err("explicit reindex must diagnose the corrupt envelope");
    assert!(
        error.to_string().contains(&selected[0].id),
        "expected the corrupt task id in `{error}`"
    );
}

/// Writers rewriting envelopes while readers select must never see a parse
/// that outlived its file: every listing agrees with the durable state.
#[test]
fn concurrent_updates_never_serve_a_superseded_cached_envelope() {
    const TASKS: usize = 6;
    const ROUNDS: usize = 25;

    let temp = TempDir::new().expect("tempdir");
    let store = corpus(&temp, TASKS);
    let listings = std::sync::atomic::AtomicUsize::new(0);

    let ids = store
        .task_candidates(&TaskListFilter::default(), TASKS)
        .expect("select ids")
        .items
        .into_iter()
        .map(|envelope| envelope.id)
        .collect::<Vec<_>>();

    std::thread::scope(|scope| {
        for (index, id) in ids.iter().enumerate() {
            let store = &store;
            scope.spawn(move || {
                for round in 0..ROUNDS {
                    store
                        .update_task_document(
                            id,
                            &TaskDocumentUpdateParams {
                                actor: "codex".to_string(),
                                title: Some(format!("writer {index} @{round}")),
                                ..Default::default()
                            },
                        )
                        .unwrap_or_else(|err| panic!("update of {id} failed: {err}"));
                }
            });
        }
        for _ in 0..3 {
            let store = &store;
            let listings = &listings;
            scope.spawn(move || {
                for _ in 0..(ROUNDS * TASKS) {
                    let candidates = store
                        .task_candidates(&TaskListFilter::default(), TASKS)
                        .unwrap_or_else(|err| panic!("selection failed: {err}"));
                    assert_eq!(candidates.total, TASKS, "no task may drop out");
                    listings.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });

    assert!(listings.load(Ordering::Relaxed) > 0);
    let candidates = store
        .task_candidates(&TaskListFilter::default(), TASKS)
        .expect("final selection");
    for envelope in candidates.items {
        let durable = store
            .get_task(&envelope.id)
            .expect("read task")
            .expect("task exists");
        assert_eq!(
            envelope.title, durable.title,
            "a cached parse outlived its file"
        );
        assert_eq!(envelope.updated_at, durable.updated_at);
    }
}

/// Acceptance-scale evidence: at 3000 registered tasks a warm selection parses
/// nothing, pays one metadata probe per task, and still agrees with a strict
/// bundle scan on filtering, ordering and totals. Ignored by default because
/// generating the corpus dominates the run.
#[test]
#[ignore = "generated 3000-task corpus"]
#[allow(clippy::print_stdout)]
fn a_warm_three_thousand_task_selection_parses_no_envelopes() {
    const TASKS: usize = 3000;

    let temp = TempDir::new().expect("tempdir");
    let store = seed(&temp.path().join("global"), "ws_warm_3000", TASKS);
    let filters = filters();
    let expected = filters
        .iter()
        .map(|filter| strict_reference(&store, filter, 50))
        .collect::<Vec<_>>();
    scan_cost(&store);

    let cold_started = Instant::now();
    let cold = store
        .task_candidates(&filters[0], 50)
        .expect("cold selection");
    let cold_ms = cold_started.elapsed().as_secs_f64() * 1000.0;
    let cold_cost = scan_cost(&store);
    assert_eq!(cold_cost, (TASKS, TASKS));
    assert_eq!(cold.total, TASKS);

    let mut warm_ms = Vec::new();
    for (filter, (expected_ids, expected_total)) in filters.iter().zip(expected) {
        let started = Instant::now();
        let candidates = store.task_candidates(filter, 50).expect("warm selection");
        warm_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(
            scan_cost(&store),
            (TASKS, 0),
            "a warm scan must stat every task and parse none"
        );
        assert_eq!(candidates.total, expected_total);
        assert_eq!(selected_ids(&candidates), expected_ids);
    }
    warm_ms.sort_by(f64::total_cmp);

    println!(
        "{}",
        serde_json::json!({"tasks": TASKS, "filters": warm_ms.len(),
        "cold_ms": cold_ms, "cold_envelope_parses": cold_cost.1,
        "cold_stat_calls": cold_cost.0, "warm_median_ms": warm_ms[warm_ms.len() / 2],
        "warm_envelope_parses": 0, "warm_stat_calls": TASKS})
    );
}
