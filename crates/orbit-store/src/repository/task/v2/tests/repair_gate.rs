//! ORB-14181: a canonical relation to a task the registry no longer has
//! blocks every automatic index rebuild. Reads must stay correct while the
//! index is degraded, attempt the same refused rebuild a bounded number of
//! times across store instances and threads, and repair on their own once a
//! supported write or a restored target changes the outcome.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};

use orbit_types::task::{TASK_EVENTS_FILE_NAME, TaskRelation, TaskRelationType};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

use super::*;
use crate::contracts::{TaskDocumentUpdateParams, TaskListFilter};
use crate::repository::task::v2::repair_gate::RepairGate;
use crate::repository::task::v2_bundle::LISTING_BUNDLE_READS;

const WORKSPACE: &str = "orbit-test-123456";
const RETIRED_WORKSPACE: &str = "orbit-test-654321";
const FOREIGN_TARGET: &str = "ZZZ-00001";
const HEALTHY_TASKS: usize = 5;

struct Fixture {
    temp: TempDir,
    registry: TaskRegistryStore,
    store: TaskV2Store,
    source: String,
    target: String,
    target_dir: std::path::PathBuf,
    in_progress: String,
}

impl Fixture {
    fn registered(&self) -> u64 {
        self.registry
            .tasks_for_workspace(WORKSPACE)
            .expect("bindings")
            .len() as u64
    }

    fn gate(&self) -> RepairGate {
        RepairGate::new(self.registry.workspaces_dir(), WORKSPACE)
    }

    /// Another store over the same partition, as a fresh request, MCP call or
    /// doctor check opens one — through its own registry handle.
    fn reopened_store(&self) -> TaskV2Store {
        let registry =
            TaskRegistryStore::open(&task_registry_path(self.temp.path())).expect("reopen");
        TaskV2Store::new(registry, WORKSPACE.to_string())
    }

    fn sql(&self, statement: &str) {
        rusqlite::Connection::open(task_registry_path(self.temp.path()))
            .expect("open registry db")
            .execute_batch(statement)
            .expect("fixture sql");
    }
}

/// The live ORB-14171 shape: a done task in this workspace keeps
/// `spawned_from` to a task whose workspace was retired, its generated
/// relation rows are gone, and the index is stale. A foreign-prefix edge and
/// an ADR `produces` edge stay allowed to dangle.
fn degraded_fixture() -> Fixture {
    let temp = TempDir::new().expect("tempdir");
    let registry =
        TaskRegistryStore::open(&task_registry_path(temp.path())).expect("open registry");
    let store = bound_store(&registry, &temp, WORKSPACE, "repo");
    let retired = bound_store(&registry, &temp, RETIRED_WORKSPACE, "bridge");

    let target = retired
        .create_task(create_params("Retired provenance", TaskStatus::Done))
        .expect("create target")
        .id;
    for index in 0..HEALTHY_TASKS {
        store
            .create_task(create_params(
                &format!("Healthy {index}"),
                TaskStatus::Backlog,
            ))
            .expect("create healthy task");
    }
    let in_progress = store
        .create_task(create_params("Active", TaskStatus::InProgress))
        .expect("create active task")
        .id;
    let mut source = create_params("Spawned", TaskStatus::Done);
    source.relations = vec![
        TaskRelation {
            relation_type: TaskRelationType::SpawnedFrom,
            target: target.clone(),
        },
        TaskRelation {
            relation_type: TaskRelationType::RelatedTo,
            target: FOREIGN_TARGET.to_string(),
        },
        TaskRelation {
            relation_type: TaskRelationType::Produces,
            target: "ADR-0001".to_string(),
        },
    ];
    let source = store.create_task(source).expect("create source").id;

    let target_dir = registry
        .find_task_binding(&target)
        .expect("lookup target")
        .expect("target bound")
        .canonical_path;
    assert!(
        registry
            .unregister_task_bundle(&target, RETIRED_WORKSPACE)
            .expect("retire target"),
        "fixture must drop the target's binding"
    );
    let fixture = Fixture {
        temp,
        registry,
        store,
        source,
        target,
        target_dir,
        in_progress,
    };
    fixture.sql(&format!(
        "DELETE FROM task_bundle_relations WHERE source_task_id = '{source}';
         DELETE FROM task_bundle_index WHERE task_id = '{source}';",
        source = fixture.source
    ));
    fixture
}

/// Listing-path bundle reads `op` performs on this thread.
fn bundle_reads<T>(op: impl FnOnce() -> T) -> (T, u64) {
    let before = LISTING_BUNDLE_READS.with(std::cell::Cell::get);
    let value = op();
    (
        value,
        LISTING_BUNDLE_READS.with(std::cell::Cell::get) - before,
    )
}

/// Run the named test alone in a child process and report `true` to the
/// parent, or `false` inside that child. A thread-scoped subscriber only
/// sees a callsite whose cached interest it took part in; a sibling test
/// registering the same `warn!` concurrently can cache it without this
/// subscriber, so warning counts are only exact without siblings.
fn ran_in_child(test: &str) -> bool {
    const CHILD: &str = "ORBIT_STORE_REPAIR_GATE_CHILD";
    if std::env::var(CHILD).ok().as_deref() == Some(test) {
        return false;
    }
    let path = format!(
        "{}::{test}",
        module_path!()
            .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
            .expect("crate-relative module path")
    );
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    command
        .args(["--exact", &path, "--test-threads=1"])
        .env(CHILD, test);
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = orbit_common::process::run_bounded_capped(
        &mut command,
        std::time::Duration::from_secs(120),
        1024 * 1024,
    )
    .expect("run isolated child");
    orbit_common::test_env::assert_child_test_passed(
        &path,
        output.status,
        &output.stdout,
        &output.stderr,
    );
    true
}

/// A subscriber and shared warning count the test can install on reader threads.
fn warning_dispatch() -> (tracing::Dispatch, Arc<AtomicU64>) {
    struct CountWarnings(Arc<AtomicU64>);
    impl<S: tracing::Subscriber> Layer<S> for CountWarnings {
        fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
            let metadata = event.metadata();
            if *metadata.level() == tracing::Level::WARN
                && metadata.target() == "orbit.store.task_v2"
            {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
    let count = Arc::new(AtomicU64::new(0));
    let subscriber = Registry::default().with(CountWarnings(Arc::clone(&count)));
    (tracing::Dispatch::new(subscriber), count)
}

/// Warnings the task store emits while `op` runs on this thread.
fn store_warnings<T>(op: impl FnOnce() -> T) -> (T, u64) {
    let (dispatch, count) = warning_dispatch();
    let value = tracing::dispatcher::with_default(&dispatch, op);
    (value, count.load(Ordering::SeqCst))
}

fn active_ids(store: &TaskV2Store) -> Vec<String> {
    store
        .list_tasks_filtered(Some(TaskStatus::InProgress), None, None, None, None, None)
        .expect("filtered listing")
        .into_iter()
        .map(|task| task.id)
        .collect()
}

#[test]
fn repeated_reads_while_degraded_attempt_one_rebuild_and_stay_correct() {
    if ran_in_child("repeated_reads_while_degraded_attempt_one_rebuild_and_stay_correct") {
        return;
    }
    let fixture = degraded_fixture();
    let registered = fixture.registered();

    let ((), warnings) = store_warnings(|| {
        for round in 0..5 {
            let (ids, reads) = bundle_reads(|| active_ids(&fixture.store));
            assert_eq!(ids, vec![fixture.in_progress.clone()], "round {round}");
            assert_eq!(
                reads, registered,
                "a degraded filtered read scans each bundle once (round {round})"
            );
        }
        let (tagged, reads) = bundle_reads(|| {
            fixture
                .store
                .list_tasks_by_tags(&["v2".to_string()])
                .expect("tag listing")
        });
        assert_eq!(tagged.len() as u64, registered);
        assert_eq!(reads, registered);

        let reopened = fixture.reopened_store();
        let (page, reads) = bundle_reads(|| {
            reopened
                .query_task_rows(
                    &TaskListFilter {
                        statuses: Some(vec![TaskStatus::Done]),
                        ..Default::default()
                    },
                    50,
                    None,
                )
                .expect("page")
        });
        let ids = page
            .items
            .iter()
            .map(|row| row.task.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![fixture.source.as_str()]);
        assert_eq!(page.total, 1);
        assert_eq!(
            reads,
            registered + 1,
            "one scan plus hydration of the selected row"
        );
    });

    assert_eq!(
        fixture.gate().attempts(),
        1,
        "the refused rebuild must not repeat per read or per store instance"
    );
    assert_eq!(fixture.gate().suppressed_reads(), 6);
    assert_eq!(warnings, 1, "one warning per refused attempt");

    // The suppression has an expiry even when nothing changed.
    fixture.gate().expire_failure();
    let (ids, warnings) = store_warnings(|| active_ids(&fixture.store));
    assert_eq!(ids, vec![fixture.in_progress.clone()]);
    assert_eq!(fixture.gate().attempts(), 2);
    assert_eq!(warnings, 1);
}

#[test]
fn concurrent_readers_across_store_instances_share_one_attempt() {
    if ran_in_child("concurrent_readers_across_store_instances_share_one_attempt") {
        return;
    }
    const READERS: usize = 8;
    const ROUNDS: u64 = 3;
    let fixture = degraded_fixture();
    let registered = fixture.registered();
    let barrier = Barrier::new(READERS);
    let (dispatch, warnings) = warning_dispatch();

    let reads = std::thread::scope(|scope| {
        let handles = (0..READERS)
            .map(|_| {
                let store = fixture.reopened_store();
                let barrier = &barrier;
                let fixture = &fixture;
                let dispatch = dispatch.clone();
                scope.spawn(move || {
                    tracing::dispatcher::with_default(&dispatch, || {
                        barrier.wait();
                        let mut reads = 0;
                        for _ in 0..ROUNDS {
                            let (ids, scanned) = bundle_reads(|| active_ids(&store));
                            assert_eq!(ids, vec![fixture.in_progress.clone()]);
                            reads += scanned;
                        }
                        reads
                    })
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("reader"))
            .collect::<Vec<_>>()
    });

    assert_eq!(fixture.gate().attempts(), 1);
    assert_eq!(
        warnings.load(Ordering::SeqCst),
        1,
        "concurrent failed repair emits one warning"
    );
    assert!(
        reads.iter().all(|reads| *reads == ROUNDS * registered),
        "every degraded read scans each bundle exactly once: {reads:?}"
    );
}

#[test]
fn degraded_reads_still_fail_on_task_field_corruption() {
    if ran_in_child("degraded_reads_still_fail_on_task_field_corruption") {
        return;
    }
    let fixture = degraded_fixture();
    assert_eq!(
        active_ids(&fixture.store),
        vec![fixture.in_progress.clone()]
    );
    assert_eq!(fixture.gate().attempts(), 1);

    let events = fixture
        .store
        .bundle_store
        .bundle_path(&fixture.in_progress)
        .expect("bundle path")
        .join(TASK_EVENTS_FILE_NAME);
    let original = std::fs::read(&events).expect("read events");
    let mut damaged = b"{not json\n".to_vec();
    damaged.extend_from_slice(&original);
    std::fs::write(&events, damaged).expect("damage events");

    assert!(
        fixture
            .store
            .list_tasks_filtered(Some(TaskStatus::InProgress), None, None, None, None, None)
            .is_err(),
        "a suppressed repair must not hide a corrupt bundle"
    );
    assert!(
        fixture
            .store
            .task_candidates(&TaskListFilter::default(), 50)
            .is_err()
    );

    std::fs::write(&events, original).expect("restore events");
    assert_eq!(
        active_ids(&fixture.store),
        vec![fixture.in_progress.clone()]
    );
    assert_eq!(fixture.gate().attempts(), 1);
}

#[test]
fn removing_the_edge_through_a_supported_update_repairs_on_the_next_read() {
    if ran_in_child("removing_the_edge_through_a_supported_update_repairs_on_the_next_read") {
        return;
    }
    let fixture = degraded_fixture();
    // A second stale row keeps the index unusable after the source's own
    // index row is rewritten, so recovery has to come from the gate.
    let (_, healthy) = fixture
        .store
        .list_tasks()
        .expect("list")
        .into_iter()
        .map(|task| task.id)
        .partition::<Vec<_>, _>(|id| *id == fixture.source || *id == fixture.in_progress);
    fixture.sql(&format!(
        "DELETE FROM task_bundle_index WHERE task_id = '{}';",
        healthy[0]
    ));
    assert_eq!(
        active_ids(&fixture.store),
        vec![fixture.in_progress.clone()]
    );
    assert_eq!(fixture.gate().attempts(), 1);

    fixture
        .store
        .update_task_document(
            &fixture.source,
            &TaskDocumentUpdateParams {
                actor: "operator".to_string(),
                relations: Some(vec![
                    TaskRelation {
                        relation_type: TaskRelationType::RelatedTo,
                        target: FOREIGN_TARGET.to_string(),
                    },
                    TaskRelation {
                        relation_type: TaskRelationType::Produces,
                        target: "ADR-0001".to_string(),
                    },
                ]),
                ..Default::default()
            },
        )
        .expect("drop the dangling edge");

    let (ids, reads) = bundle_reads(|| active_ids(&fixture.reopened_store()));
    assert_eq!(ids, vec![fixture.in_progress.clone()]);
    assert_eq!(fixture.gate().attempts(), 2, "the changed source re-admits");
    assert_eq!(reads, fixture.registered());
    assert!(fixture.gate().unresolved_targets().is_empty());

    let (ids, reads) = bundle_reads(|| active_ids(&fixture.store));
    assert_eq!(ids, vec![fixture.in_progress.clone()]);
    assert_eq!(reads, 1, "the repaired index serves the filter");
    assert_eq!(fixture.gate().attempts(), 2);
    assert!(
        fixture
            .registry
            .unresolved_relation_targets(
                WORKSPACE,
                &[fixture
                    .store
                    .bundle_store
                    .read_envelope_if_settled(&fixture.source)
                    .expect("read source")
                    .expect("source settled")]
            )
            .expect("audit")
            .is_empty()
    );
}

#[test]
fn restoring_the_target_repairs_on_the_next_read() {
    if ran_in_child("restoring_the_target_repairs_on_the_next_read") {
        return;
    }
    let fixture = degraded_fixture();
    assert_eq!(
        active_ids(&fixture.store),
        vec![fixture.in_progress.clone()]
    );
    assert_eq!(
        active_ids(&fixture.store),
        vec![fixture.in_progress.clone()]
    );
    assert_eq!(fixture.gate().attempts(), 1);
    assert_eq!(
        fixture.gate().unresolved_targets(),
        std::collections::BTreeSet::from([fixture.target.clone()])
    );

    fixture
        .registry
        .register_task_bundle(&fixture.target, RETIRED_WORKSPACE, &fixture.target_dir)
        .expect("restore target binding");

    assert_eq!(
        active_ids(&fixture.store),
        vec![fixture.in_progress.clone()]
    );
    assert_eq!(
        fixture.gate().attempts(),
        2,
        "the restored target re-admits"
    );
    let (ids, reads) = bundle_reads(|| active_ids(&fixture.store));
    assert_eq!(ids, vec![fixture.in_progress.clone()]);
    assert_eq!(reads, 1, "the repaired index serves the filter");
    assert_eq!(fixture.gate().attempts(), 2);
}

#[test]
fn rebuild_refusal_names_the_canonical_source_edge() {
    if ran_in_child("rebuild_refusal_names_the_canonical_source_edge") {
        return;
    }
    let fixture = degraded_fixture();
    let envelopes = fixture
        .store
        .bundle_store
        .list_bundles()
        .expect("bundles")
        .into_iter()
        .map(|bundle| bundle.envelope)
        .collect::<Vec<_>>();

    let error = fixture
        .registry
        .replace_workspace_task_indexes(WORKSPACE, &envelopes)
        .expect_err("validator stays strict");
    assert!(
        error.to_string().contains(&format!(
            "{} spawned_from -> '{}'",
            fixture.source, fixture.target
        )),
        "the refusal must name the source and relation: {error}"
    );

    let unresolved = fixture
        .registry
        .unresolved_relation_targets(WORKSPACE, &envelopes)
        .expect("audit");
    assert_eq!(
        unresolved
            .iter()
            .map(|edge| (
                edge.source_task_id.as_str(),
                edge.relation_type.as_str(),
                edge.target_task_id.as_str(),
                edge.indexed
            ))
            .collect::<Vec<_>>(),
        vec![(
            fixture.source.as_str(),
            "spawned_from",
            fixture.target.as_str(),
            false
        )],
        "only the locally known task edge is unresolved, and the index lacks it"
    );
}
