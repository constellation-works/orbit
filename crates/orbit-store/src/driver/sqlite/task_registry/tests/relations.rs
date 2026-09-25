//! Relation validation: cross-workspace targets, cycles, dangling targets and
//! the relation index queries.

use std::cell::RefCell;
use std::fs;

use orbit_types::task::{TaskRelation, TaskRelationType, TaskStatus};
use rusqlite::{Connection, params, params_from_iter};
use tempfile::TempDir;

use super::super::allocator::TASK_PREFIX_PROBE_SQL;
use super::super::relations::reachable_cycle_family_sql;
use super::super::{BindWorkspaceParams, RegisterWorkspaceParams, TaskRegistryStore};
use super::{bind, create_canonical_bundle, envelope, store};

thread_local! {
    static TRACED_SQL: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn record_traced_sql(sql: &str) {
    TRACED_SQL.with(|traced| traced.borrow_mut().push(sql.to_string()));
}

fn take_traced_sql() -> Vec<String> {
    TRACED_SQL.with(|traced| std::mem::take(&mut *traced.borrow_mut()))
}

#[test]
fn checkoutless_workspaces_coordinate_cross_workspace_relations_without_paths() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let first = store
        .register_workspace(RegisterWorkspaceParams {
            partition_id: "logical-first-aaaaaa".into(),
            slug: "Logical First".into(),
            repo_fingerprint: Some("first-fingerprint".into()),
        })
        .expect("register first logical workspace");
    let second = store
        .register_workspace(RegisterWorkspaceParams {
            partition_id: "logical-second-bbbbbb".into(),
            slug: "Logical Second".into(),
            repo_fingerprint: None,
        })
        .expect("register second logical workspace");

    assert!(
        store
            .find_workspace_checkout(&first.partition_id)
            .expect("find first checkout")
            .is_none()
    );
    assert!(
        store
            .find_workspace_checkout(&second.partition_id)
            .expect("find second checkout")
            .is_none()
    );

    let target_id = store
        .allocate_task_id(&second.partition_id)
        .expect("allocate target");
    let source_id = store
        .allocate_task_id(&first.partition_id)
        .expect("allocate source");
    assert_eq!(
        (target_id.as_str(), source_id.as_str()),
        ("ORB-00000", "ORB-00001")
    );

    for (workspace_id, task_id) in [
        (second.partition_id.as_str(), target_id.as_str()),
        (first.partition_id.as_str(), source_id.as_str()),
    ] {
        let path = store
            .canonical_task_bundle_path(workspace_id, task_id)
            .expect("canonical coordination path");
        fs::create_dir_all(&path).expect("create canonical bundle");
        store
            .register_task_bundle(task_id, workspace_id, &path)
            .expect("register canonical bundle");
    }
    store
        .replace_task_index(
            &second.partition_id,
            &envelope(&target_id, TaskStatus::Done, Vec::new(), Vec::new()),
        )
        .expect("index completed target");
    store
        .replace_task_index(
            &first.partition_id,
            &envelope(
                &source_id,
                TaskStatus::Backlog,
                Vec::new(),
                vec![
                    TaskRelation {
                        relation_type: TaskRelationType::BlockedBy,
                        target: target_id.clone(),
                    },
                    TaskRelation {
                        relation_type: TaskRelationType::RelatedTo,
                        target: target_id.clone(),
                    },
                ],
            ),
        )
        .expect("index cross-workspace relations");

    assert_eq!(
        store
            .global_task_status_index()
            .expect("global statuses")
            .get(&target_id),
        Some(&TaskStatus::Done)
    );
    assert_eq!(
        store
            .indexed_relation_targets(&first.partition_id, &source_id, TaskRelationType::BlockedBy,)
            .expect("cross-workspace dependency"),
        vec![target_id.clone()]
    );
    assert_eq!(
        store
            .indexed_task_count_for_workspace(&first.partition_id)
            .expect("first count"),
        1
    );
    assert_eq!(
        store
            .indexed_task_count_for_workspace(&second.partition_id)
            .expect("second count"),
        1
    );

    let before_allocator = store.allocator_next_number().expect("allocator before");
    let missing_target = "ORB-09999";
    let error = store
        .validate_new_task_relation_targets(
            &first.partition_id,
            &[TaskRelation {
                relation_type: TaskRelationType::BlockedBy,
                target: missing_target.into(),
            }],
        )
        .expect_err("missing global target");
    assert!(error.to_string().contains(missing_target));
    assert!(error.to_string().contains(&first.partition_id));
    assert_eq!(
        store.allocator_next_number().expect("allocator after"),
        before_allocator,
        "missing target preflight must not consume an ID"
    );

    let error = store
        .replace_task_index(
            &first.partition_id,
            &envelope(
                &source_id,
                TaskStatus::Review,
                Vec::new(),
                vec![TaskRelation {
                    relation_type: TaskRelationType::RelatedTo,
                    target: missing_target.into(),
                }],
            ),
        )
        .expect_err("missing relation target");
    assert!(error.to_string().contains(missing_target));
    assert!(error.to_string().contains(&first.partition_id));
    assert_eq!(
        store
            .global_task_status_index()
            .expect("status after rejected update")
            .get(&source_id),
        Some(&TaskStatus::Backlog),
        "rejected relation update must be atomic"
    );
    assert_eq!(
        store
            .indexed_relation_targets(&first.partition_id, &source_id, TaskRelationType::BlockedBy,)
            .expect("relation after rejected update"),
        vec![target_id]
    );

    let foreign_target = "DK-00042";
    store
        .validate_new_task_relation_targets(
            &first.partition_id,
            &[TaskRelation {
                relation_type: TaskRelationType::BlockedBy,
                target: foreign_target.into(),
            }],
        )
        .expect("foreign-prefix target cannot be verified locally");
    store
        .replace_task_index(
            &first.partition_id,
            &envelope(
                &source_id,
                TaskStatus::Review,
                Vec::new(),
                vec![
                    TaskRelation {
                        relation_type: TaskRelationType::BlockedBy,
                        target: foreign_target.into(),
                    },
                    TaskRelation {
                        relation_type: TaskRelationType::RelatedTo,
                        target: foreign_target.into(),
                    },
                ],
            ),
        )
        .expect("index foreign-prefix relations");
    assert_eq!(
        store
            .indexed_relation_targets(&first.partition_id, &source_id, TaskRelationType::RelatedTo)
            .expect("foreign relation target"),
        vec![foreign_target.to_string()]
    );
    assert!(
        store
            .dangling_relation_targets(Some(&first.partition_id))
            .expect("audit foreign target")
            .is_empty(),
        "allowed foreign references are not locally dangling"
    );
}

/// Relation validation reads a reachable subgraph rather than the whole
/// relation table, and the walk has to cross workspace boundaries: a relation
/// may target a task in another workspace, and a cycle routed through one is
/// still a cycle. Scoping the query to the writing workspace would pass this
/// write.
#[test]
fn relation_cycle_through_another_workspace_is_rejected() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let first = store
        .register_workspace(RegisterWorkspaceParams {
            partition_id: "logical-first-aaaaaa".into(),
            slug: "Logical First".into(),
            repo_fingerprint: None,
        })
        .expect("register first workspace");
    let second = store
        .register_workspace(RegisterWorkspaceParams {
            partition_id: "logical-second-bbbbbb".into(),
            slug: "Logical Second".into(),
            repo_fingerprint: None,
        })
        .expect("register second workspace");

    // `head` and `tail` live in the first workspace, `bridge` in the second,
    // so the only path from `tail` back to `head` leaves and re-enters.
    let head = register_indexed_task(&store, &first.partition_id, Vec::new());
    let bridge = register_indexed_task(&store, &second.partition_id, Vec::new());
    let tail = register_indexed_task(&store, &first.partition_id, Vec::new());

    store
        .replace_task_index(
            &second.partition_id,
            &envelope(
                &bridge,
                TaskStatus::Backlog,
                Vec::new(),
                vec![blocked_by(&tail)],
            ),
        )
        .expect("index bridge -> tail");
    store
        .replace_task_index(
            &first.partition_id,
            &envelope(
                &head,
                TaskStatus::Backlog,
                Vec::new(),
                vec![blocked_by(&bridge)],
            ),
        )
        .expect("index head -> bridge");

    let error = store
        .replace_task_index(
            &first.partition_id,
            &envelope(
                &tail,
                TaskStatus::Backlog,
                Vec::new(),
                vec![blocked_by(&head)],
            ),
        )
        .expect_err("cycle closing through the second workspace");
    assert!(
        error.to_string().contains("cycle"),
        "expected a cycle rejection, got: {error}"
    );
    assert!(
        store
            .indexed_relation_targets(&first.partition_id, &tail, TaskRelationType::BlockedBy)
            .expect("relations after rejected cycle")
            .is_empty(),
        "a rejected cycle must not write relation rows"
    );
}

/// A replacement batch's own edges are not in the registry yet, so a path that
/// crosses one resumes at its target — and the stored edges *after* that point
/// are only fetched if the subgraph walk is seeded on replacement targets too.
///
/// The cycle here is `s -> t -> x -> y -> s`, where `t -> x` and `y -> s` are
/// stored and `x -> y` / `s -> t` arrive in one batch. Reaching the stored
/// `y -> s` requires crossing the batch's own unwritten `x -> y`, so seeding
/// only on the new relations' targets admits the cycle.
#[test]
fn relation_cycle_resuming_after_a_replacement_edge_is_rejected() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());
    let partition_id = workspace.partition_id.clone();

    let x = register_indexed_task(&store, &partition_id, Vec::new());
    let s = register_indexed_task(&store, &partition_id, Vec::new());
    let t = register_indexed_task(&store, &partition_id, vec![blocked_by(&x)]);
    let y = register_indexed_task(&store, &partition_id, vec![blocked_by(&s)]);

    let error = store
        .replace_task_indexes(
            &partition_id,
            &[
                envelope(&x, TaskStatus::Backlog, Vec::new(), vec![blocked_by(&y)]),
                envelope(&s, TaskStatus::Backlog, Vec::new(), vec![blocked_by(&t)]),
            ],
        )
        .expect_err("cycle resuming after a replacement edge");
    assert!(
        error.to_string().contains("cycle"),
        "expected a cycle rejection, got: {error}"
    );
    assert!(
        store
            .indexed_relation_targets(&partition_id, &x, TaskRelationType::BlockedBy)
            .expect("relations after rejected batch")
            .is_empty(),
        "a rejected batch must not write relation rows"
    );
}

/// Target resolution probes one prefix at a time instead of collecting every
/// registered prefix. A prefix that is registered but not the active minting
/// prefix must still make an unresolvable target an error, the way a foreign
/// prefix does not.
#[test]
fn unresolvable_target_under_a_registered_foreign_prefix_is_rejected() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());
    let partition_id = workspace.partition_id.clone();
    let source_id = register_indexed_task(&store, &partition_id, Vec::new());

    let mirrored = "DK-00001";
    let mirrored_path = store
        .canonical_task_bundle_path(&partition_id, mirrored)
        .expect("canonical path for mirrored task");
    fs::create_dir_all(&mirrored_path).expect("create mirrored bundle");
    store
        .register_task_bundle(mirrored, &partition_id, &mirrored_path)
        .expect("register mirrored bundle");

    let missing = "DK-00002";
    let error = store
        .replace_task_index(
            &partition_id,
            &envelope(
                &source_id,
                TaskStatus::Backlog,
                Vec::new(),
                vec![TaskRelation {
                    relation_type: TaskRelationType::RelatedTo,
                    target: missing.into(),
                }],
            ),
        )
        .expect_err("unresolvable target under a registered prefix");
    assert!(error.to_string().contains(missing));

    store
        .replace_task_index(
            &partition_id,
            &envelope(
                &source_id,
                TaskStatus::Backlog,
                Vec::new(),
                vec![TaskRelation {
                    relation_type: TaskRelationType::RelatedTo,
                    target: mirrored.into(),
                }],
            ),
        )
        .expect("registered target under the same prefix resolves");
}

/// Both relation-validation queries must resolve through an index. The cost
/// they replaced grew with every relation row and every binding in the
/// registry, across all workspaces, on each task write — a plan that falls
/// back to a scan puts that cost straight back.
#[test]
fn relation_subgraph_query_stays_indexed() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let conn = store.conn.lock().expect("lock registry");

    let plan = query_plan(&conn, &reachable_cycle_family_sql(2));
    assert!(
        !plan.contains("SCAN edge"),
        "the relation subgraph walk must not scan task_bundle_relations:\n{plan}"
    );
    assert_eq!(
        plan.matches("SEARCH edge USING").count(),
        2,
        "both the recursive step and the collecting select must search by source id:\n{plan}"
    );

    let plan = query_plan(&conn, TASK_PREFIX_PROBE_SQL);
    assert!(
        plan.contains("SEARCH task_bundle_bindings USING"),
        "the prefix probe must search the bindings index, not scan it:\n{plan}"
    );
}

fn query_plan(conn: &Connection, sql: &str) -> String {
    let mut stmt = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("prepare query plan");
    // SQLite fixes the plan at prepare time, so the bound values are
    // irrelevant — they only have to be present for the statement to run.
    let bindings = vec![String::new(); stmt.parameter_count()];
    let rows = stmt
        .query_map(params_from_iter(bindings.iter()), |row| {
            row.get::<_, String>(3)
        })
        .expect("query plan rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect query plan");
    rows.join("\n")
}

fn blocked_by(target: &str) -> TaskRelation {
    TaskRelation {
        relation_type: TaskRelationType::BlockedBy,
        target: target.to_string(),
    }
}

/// Allocate, register, and index one task, returning its id.
fn register_indexed_task(
    store: &TaskRegistryStore,
    partition_id: &str,
    relations: Vec<TaskRelation>,
) -> String {
    let task_id = store
        .allocate_task_id(partition_id)
        .expect("allocate task id");
    let path = store
        .canonical_task_bundle_path(partition_id, &task_id)
        .expect("canonical bundle path");
    fs::create_dir_all(&path).expect("create bundle");
    store
        .register_task_bundle(&task_id, partition_id, &path)
        .expect("register bundle");
    store
        .replace_task_index(
            partition_id,
            &envelope(&task_id, TaskStatus::Backlog, Vec::new(), relations),
        )
        .expect("index task");
    task_id
}

#[test]
fn dangling_relation_targets_reports_only_grandfathered_orb_targets() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());

    // A resolvable target and the source both exist in the registry.
    let target_id = store
        .allocate_task_id(&workspace.partition_id)
        .expect("allocate target");
    let source_id = store
        .allocate_task_id(&workspace.partition_id)
        .expect("allocate source");
    for task_id in [target_id.as_str(), source_id.as_str()] {
        let path = store
            .canonical_task_bundle_path(&workspace.partition_id, task_id)
            .expect("canonical bundle path");
        fs::create_dir_all(&path).expect("create canonical bundle");
        store
            .register_task_bundle(task_id, &workspace.partition_id, &path)
            .expect("register bundle");
    }
    store
        .replace_task_index(
            &workspace.partition_id,
            &envelope(&target_id, TaskStatus::Done, Vec::new(), Vec::new()),
        )
        .expect("index target");
    // Source carries a resolvable ORB relation plus a non-ORB `resolves`
    // target; the audit must flag neither.
    store
        .replace_task_index(
            &workspace.partition_id,
            &envelope(
                &source_id,
                TaskStatus::Backlog,
                Vec::new(),
                vec![
                    TaskRelation {
                        relation_type: TaskRelationType::RelatedTo,
                        target: target_id.clone(),
                    },
                    TaskRelation {
                        relation_type: TaskRelationType::Resolves,
                        target: "F2026-05-001".into(),
                    },
                ],
            ),
        )
        .expect("index source relations");

    assert!(
        store
            .dangling_relation_targets(None)
            .expect("audit clean")
            .is_empty(),
        "resolvable + non-ORB targets must not be flagged"
    );

    // Grandfather a dangling ORB target. The public API forbids adding one (the
    // validator rejects it at index time), so these only exist as legacy rows —
    // inject one directly to reproduce that state.
    let missing_target = "ORB-09999";
    {
        let conn = store.conn.lock().expect("lock registry");
        conn.execute(
            "INSERT INTO task_bundle_relations(
                source_task_id, workspace_id, relation_type, target_task_id
            ) VALUES (?1, ?2, ?3, ?4)",
            params![
                source_id,
                workspace.partition_id,
                "related_to",
                missing_target
            ],
        )
        .expect("seed grandfathered relation");
    }

    let dangling = store
        .dangling_relation_targets(None)
        .expect("audit dangling");
    assert_eq!(
        dangling.len(),
        1,
        "only the missing ORB target dangles: {dangling:?}"
    );
    assert_eq!(dangling[0].source_task_id, source_id);
    assert_eq!(dangling[0].target_task_id, missing_target);
    assert_eq!(dangling[0].relation_type, "related_to");
    assert_eq!(dangling[0].partition_id, workspace.partition_id);
    assert_eq!(
        store
            .dangling_relation_targets(Some(&workspace.partition_id))
            .expect("scoped audit")
            .len(),
        1
    );

    // A second workspace with its own grandfathered target: the unscoped audit
    // spans both, each scoped audit sees exactly its own.
    let repo_two = temp.path().join("repo-two");
    let orbit_two = repo_two.join(".orbit");
    fs::create_dir_all(&orbit_two).expect("create second orbit dir");
    let workspace_two = store
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some("orbit-two-654321".into()),
            slug: "Orbit Two".into(),
            repo_root: repo_two.clone(),
            workspace_path: repo_two.clone(),
            orbit_dir: orbit_two,
            repo_fingerprint: None,
        })
        .expect("bind second workspace");
    let source_two = store
        .allocate_task_id(&workspace_two.partition_id)
        .expect("allocate second source");
    let path_two = store
        .canonical_task_bundle_path(&workspace_two.partition_id, &source_two)
        .expect("second canonical path");
    fs::create_dir_all(&path_two).expect("create second bundle");
    store
        .register_task_bundle(&source_two, &workspace_two.partition_id, &path_two)
        .expect("register second source");
    store
        .replace_task_index(
            &workspace_two.partition_id,
            &envelope(&source_two, TaskStatus::Backlog, Vec::new(), Vec::new()),
        )
        .expect("index second source");
    {
        let conn = store.conn.lock().expect("lock registry");
        conn.execute(
            "INSERT INTO task_bundle_relations(
                source_task_id, workspace_id, relation_type, target_task_id
            ) VALUES (?1, ?2, ?3, ?4)",
            params![
                source_two,
                workspace_two.partition_id,
                "blocked_by",
                "ORB-08888"
            ],
        )
        .expect("seed second grandfathered relation");
    }

    assert_eq!(
        store
            .dangling_relation_targets(None)
            .expect("audit both")
            .len(),
        2,
        "unscoped audit spans workspaces"
    );
    assert_eq!(
        store
            .dangling_relation_targets(Some(&workspace_two.partition_id))
            .expect("scoped to second")
            .len(),
        1
    );
    assert_eq!(
        store
            .dangling_relation_targets(Some(&workspace.partition_id))
            .expect("scoped to first")
            .len(),
        1
    );
}

#[test]
fn workspace_rebuild_uses_constant_relation_validation_queries() {
    const TASK_COUNT: usize = 128;

    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());
    let task_ids = (0..TASK_COUNT)
        .map(|number| format!("ORB-{number:05}"))
        .collect::<Vec<_>>();
    let bundles = task_ids
        .iter()
        .map(|task_id| {
            (
                task_id.clone(),
                create_canonical_bundle(&store, &workspace, task_id),
            )
        })
        .collect::<Vec<_>>();
    store
        .register_task_bundles(&workspace.partition_id, &bundles)
        .expect("register task bundles");

    let envelopes = task_ids
        .iter()
        .enumerate()
        .map(|(index, task_id)| {
            let relations = task_ids
                .get(index + 1)
                .map(|target| vec![blocked_by(target)])
                .unwrap_or_default();
            envelope(
                task_id,
                TaskStatus::Backlog,
                vec!["rebuilt".into()],
                relations,
            )
        })
        .collect::<Vec<_>>();

    take_traced_sql();
    {
        let mut conn = store.conn.lock().expect("lock registry");
        conn.trace(Some(record_traced_sql));
    }
    store
        .replace_workspace_task_indexes(&workspace.partition_id, &envelopes)
        .expect("rebuild workspace index");
    {
        let mut conn = store.conn.lock().expect("lock registry");
        conn.trace(None);
    }

    let traced = take_traced_sql();
    let relation_and_binding_reads = traced
        .iter()
        .filter(|sql| {
            let sql = sql.trim_start();
            (sql.starts_with("SELECT") || sql.starts_with("WITH"))
                && (sql.contains("task_bundle_relations") || sql.contains("task_bundle_bindings"))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        relation_and_binding_reads.len(),
        3,
        "a 128-envelope rebuild must issue one registered-id read, one batched target read, and one relation-subgraph read:\n{}",
        relation_and_binding_reads
            .iter()
            .map(|sql| sql.as_str())
            .collect::<Vec<_>>()
            .join("\n---\n")
    );
    assert_eq!(
        store
            .indexed_task_count_for_workspace(&workspace.partition_id)
            .expect("indexed task count"),
        TASK_COUNT
    );
    assert_eq!(
        store
            .indexed_relation_targets(
                &workspace.partition_id,
                &task_ids[0],
                TaskRelationType::BlockedBy,
            )
            .expect("first task relation"),
        vec![task_ids[1].clone()]
    );
}

#[test]
fn generated_relation_index_supports_forward_and_inverse_lookup() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());
    for task_id in ["ORB-00000", "ORB-00001", "ORB-00002"] {
        let bundle_dir = create_canonical_bundle(&store, &workspace, task_id);
        store
            .register_task_bundle(task_id, &workspace.partition_id, &bundle_dir)
            .expect("register bundle");
    }

    store
        .replace_task_index(
            &workspace.partition_id,
            &envelope(
                "ORB-00000",
                TaskStatus::Backlog,
                Vec::new(),
                vec![
                    TaskRelation {
                        relation_type: TaskRelationType::BlockedBy,
                        target: "ORB-00001".to_string(),
                    },
                    TaskRelation {
                        relation_type: TaskRelationType::RelatedTo,
                        target: "ORB-00002".to_string(),
                    },
                ],
            ),
        )
        .expect("index relations");

    assert_eq!(
        store
            .indexed_relation_targets(
                &workspace.partition_id,
                "ORB-00000",
                TaskRelationType::BlockedBy,
            )
            .expect("targets"),
        vec!["ORB-00001"]
    );
    assert_eq!(
        store
            .indexed_relation_sources(
                &workspace.partition_id,
                "ORB-00001",
                TaskRelationType::BlockedBy,
            )
            .expect("sources"),
        vec!["ORB-00000"]
    );
}
