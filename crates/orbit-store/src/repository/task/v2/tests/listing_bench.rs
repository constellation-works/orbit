//! Reproducible warm-cache storage benchmark on generated corpora (ORB-11205).
//! Run ignored test with ORBIT_TASK_BENCH_MODE=baseline|candidate and
//! ORBIT_TASK_BENCH_SIZE=100|1000|10000. Optional ORBIT_TASK_BENCH_ROOT retains
//! generated fixtures for the dashboard response benchmark; never use live data.

use std::time::Instant;

use super::listing::{probes, reads};
use super::*;
use crate::contracts::{RegisterWorkspaceParams, TaskListFilter, TaskRow};

/// Frozen settled-index algorithm from 424529c5, avoiding the candidate's
/// envelope-vector allocation when measuring the baseline.
fn baseline_list(store: &TaskV2Store, tags: &[String]) -> Vec<Task> {
    let registered = store
        .registry
        .tasks_for_workspace(&store.workspace_id)
        .unwrap();
    let indexed = store
        .registry
        .indexed_task_versions_for_workspace(&store.workspace_id)
        .unwrap();
    assert_eq!(registered.len(), indexed.len());
    for binding in registered {
        let envelope = store
            .bundle_store
            .read_envelope_if_settled(&binding.task_id)
            .unwrap()
            .unwrap();
        assert_eq!(envelope.updated_at.to_rfc3339(), indexed[&binding.task_id]);
    }
    let ids = store
        .registry
        .indexed_task_ids_filtered(
            &store.workspace_id,
            &TaskIndexFilter {
                status: None,
                priority: None,
                job_run_id: None,
                tags: tags.to_vec(),
            },
        )
        .unwrap();
    ids.iter()
        .filter_map(|id| store.bundle_store.read_bundle_if_settled(id).unwrap())
        .map(|bundle| store.task_from_bundle(bundle).unwrap())
        .collect()
}

fn baseline_row(store: &TaskV2Store, task: Task) -> TaskRow {
    TaskRow {
        comments: store.get_task_comments(&task.id).unwrap().unwrap(),
        history: store.get_task_history(&task.id).unwrap().unwrap(),
        artifacts: store.get_task_artifact_manifest(&task.id).unwrap().unwrap(),
        task,
    }
}

/// Generate `count` registered, indexed bundles for `workspace`. Shared with
/// the envelope-cache tests, which need a corpus far larger than `create_task`
/// can produce in test time.
pub(super) fn seed(global: &Path, workspace: &str, count: usize) -> TaskV2Store {
    let registry = TaskRegistryStore::open(&task_registry_path(global)).unwrap();
    registry
        .register_workspace(RegisterWorkspaceParams {
            workspace_id: workspace.to_string(),
            slug: workspace.to_string(),
            repo_fingerprint: None,
        })
        .unwrap();
    let repo = global.join(workspace);
    let orbit_dir = repo.join(".orbit");
    fs::create_dir_all(&orbit_dir).unwrap();
    fs::write(
        orbit_dir.join("config.yaml"),
        format!("schema_version: 1\nworkspace_id: {workspace}\n"),
    )
    .unwrap();
    fs::write(orbit_dir.join("config.toml"), "").unwrap();
    registry
        .bind_workspace(BindWorkspaceParams {
            workspace_id: Some(workspace.to_string()),
            slug: workspace.to_string(),
            repo_root: repo.clone(),
            workspace_path: repo,
            orbit_dir,
            repo_fingerprint: None,
        })
        .unwrap();
    let store = TaskV2Store::new(registry, workspace.to_string());
    for index in 0..1 {
        let mut params = create_params(&format!("Task {index}"), TaskStatus::Backlog);
        params.description =
            "Requirements, observed behavior, evidence, implementation detail and validation.\n"
                .repeat(100);
        params.plan = "Implement and verify the expected behavior.\n".repeat(30);
        params.comments = (0..8)
            .map(|n| TaskComment {
                at: Utc::now(),
                by: "codex".to_string(),
                message: format!("Review observation {n}: {}", "evidence ".repeat(30)),
            })
            .collect();
        if index % 10 == 0 {
            params.tags.push("selective".to_string());
        }
        let task = store.create_task(params).unwrap();
        store
            .upsert_task_artifacts(
                &task.id,
                &TaskArtifactUpdateParams {
                    owner_run_id: None,
                    actor: "codex".to_string(),
                    upsert_artifacts: vec![TaskArtifact {
                        path: "proof.txt".to_string(),
                        media_type: "text/plain".to_string(),
                        content: vec![b'x'; 1024],
                        created_by: None,
                    }],
                },
            )
            .unwrap();
        let bundle = store.bundle_store.read_bundle(&task.id).unwrap();
        let events = (0..12)
            .map(|n| {
                let mut event = bundle.events[0].clone();
                event.event_id = format!("EV-{:04}", n + 1);
                event.note = Some("Implementation evidence and validation results. ".repeat(12));
                serde_json::to_string(&event).unwrap()
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(
            store
                .bundle_store
                .bundle_path(&task.id)
                .unwrap()
                .join("events.jsonl"),
            events,
        )
        .unwrap();
    }
    // Clone a validated fixture through the file driver, allocate real IDs and
    // register bindings. Copy its generated index row directly in this test
    // fixture to avoid measuring quadratic relation admission during setup.
    let seed_id = store.registry.tasks_for_workspace(workspace).unwrap()[0]
        .task_id
        .clone();
    let source = store.bundle_store.bundle_path(&seed_id).unwrap();
    let template = store.bundle_store.read_bundle(&seed_id).unwrap();
    let conn = rusqlite::Connection::open(task_registry_path(global)).unwrap();
    for index in 1..count {
        let mut bundle = template.clone();
        let id = store.registry.allocate_task_id(workspace).unwrap();
        bundle.envelope.id = id.clone();
        bundle.envelope.title = format!("Task {index}");
        bundle.envelope.created_at = Utc::now();
        bundle.envelope.updated_at = bundle.envelope.created_at;
        if index % 10 != 0 {
            bundle.envelope.tags.retain(|tag| tag != "selective");
        }
        for event in &mut bundle.events {
            event.at = bundle.envelope.created_at;
        }
        let path = store.bundle_store.bundle_path(&id).unwrap();
        crate::driver::file::task_bundle::write_bundle_with_artifacts_at(&path, &bundle, &source)
            .unwrap();
        store
            .registry
            .register_task_bundle(&id, workspace, &path)
            .unwrap();
        conn.execute("INSERT INTO task_bundle_index (task_id, workspace_id, status, priority, job_run_id,
            created_at, updated_at, terminal_month, complexity)
            SELECT ?1, workspace_id, status, priority, job_run_id, ?2, ?2, terminal_month, complexity
            FROM task_bundle_index WHERE task_id = ?3",
            rusqlite::params![id, bundle.envelope.updated_at.to_rfc3339(), seed_id]).unwrap();
        for tag in &bundle.envelope.tags {
            conn.execute(
                "INSERT INTO task_bundle_tags (task_id, workspace_id, tag) VALUES (?1, ?2, ?3)",
                rusqlite::params![id, workspace, tag],
            )
            .unwrap();
        }
    }
    reads(&store);
    probes(&store);
    store
}

fn operation(stores: &[TaskV2Store], mode: &str, operation: &str, detail_id: &str) -> Vec<TaskRow> {
    let tags = if operation == "selective" {
        vec!["selective".to_string()]
    } else {
        Vec::new()
    };
    if operation == "detail" {
        let id = detail_id;
        let _statuses = stores[0].task_status_index().unwrap();
        return vec![if mode == "baseline" {
            baseline_row(&stores[0], stores[0].get_task(id).unwrap().unwrap())
        } else {
            stores[0].get_task_row(id, false).unwrap().unwrap()
        }];
    }
    if operation != "aggregate" {
        return if mode == "baseline" {
            let _statuses = stores[0].task_status_index().unwrap();
            baseline_list(&stores[0], &tags)
                .into_iter()
                .take(50)
                .map(|task| baseline_row(&stores[0], task))
                .collect()
        } else {
            stores[0]
                .query_task_rows(
                    &TaskListFilter {
                        tags,
                        ..Default::default()
                    },
                    50,
                    None,
                )
                .unwrap()
                .items
        };
    }
    if mode == "baseline" {
        let mut rows = Vec::new();
        for store in stores {
            let _statuses = store.task_status_index().unwrap();
            rows.extend(
                baseline_list(store, &[])
                    .into_iter()
                    .take(50)
                    .map(|task| baseline_row(store, task)),
            );
        }
        rows.sort_by(|a, b| {
            b.task
                .created_at
                .cmp(&a.task.created_at)
                .then_with(|| a.task.id.cmp(&b.task.id))
        });
        rows.truncate(50);
        rows
    } else {
        let mut candidates = Vec::new();
        for (index, store) in stores.iter().enumerate() {
            candidates.extend(
                store
                    .task_candidates(&TaskListFilter::default(), 50)
                    .unwrap()
                    .items
                    .into_iter()
                    .map(|task| (index, task)),
            );
        }
        candidates.sort_by(|(_, a), (_, b)| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        candidates.truncate(50);
        let _statuses = stores[0].task_status_index().unwrap();
        candidates
            .into_iter()
            .map(|(index, task)| stores[index].get_task_row(&task.id, true).unwrap().unwrap())
            .collect()
    }
}

#[test]
#[ignore = "generated corpus warm-cache benchmark"]
#[allow(clippy::print_stdout)]
fn task_list_io_benchmark() {
    let mode = std::env::var("ORBIT_TASK_BENCH_MODE").unwrap_or_else(|_| "candidate".to_string());
    assert!(["baseline", "candidate"].contains(&mode.as_str()));
    let size: usize = std::env::var("ORBIT_TASK_BENCH_SIZE")
        .unwrap_or_else(|_| "100".to_string())
        .parse()
        .unwrap();
    let temp = TempDir::new().unwrap();
    let global = std::env::var_os("ORBIT_TASK_BENCH_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| temp.path().join("global"));
    assert!(
        !global.exists(),
        "benchmark requires a fresh, generated root"
    );
    let stores: Vec<_> = (0..3)
        .map(|index| seed(&global, &format!("ws_bench_{index}"), size))
        .collect();
    let detail_id = stores[0]
        .registry
        .tasks_for_workspace(&stores[0].workspace_id)
        .unwrap()[0]
        .task_id
        .clone();
    for name in ["list", "selective", "detail", "aggregate"] {
        std::hint::black_box(operation(&stores, &mode, name, &detail_id));
        stores.iter().for_each(|store| {
            reads(store);
            probes(store);
        });
        let mut latencies = Vec::new();
        let mut counts = Vec::new();
        for _ in 0..11 {
            let start = Instant::now();
            std::hint::black_box(operation(&stores, &mode, name, &detail_id));
            latencies.push(start.elapsed().as_secs_f64() * 1000.0);
            counts.push(
                stores
                    .iter()
                    .map(|store| {
                        let (bundles, envelopes) = reads(store);
                        (bundles, envelopes, probes(store))
                    })
                    .fold((0, 0, 0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2)),
            );
        }
        latencies.sort_by(f64::total_cmp);
        assert!(counts.iter().all(|count| *count == counts[0]));
        let rss = fs::read_to_string("/proc/self/status").unwrap();
        let hwm = rss.lines().find(|line| line.starts_with("VmHWM:")).unwrap();
        println!(
            "{}",
            serde_json::json!({"mode": mode, "per_workspace": size, "workspaces": 3,
            "operation": name, "median_ms": latencies[5], "p95_ms": latencies[10],
            "full_bundle_loads": counts[0].0, "envelope_parses": counts[0].1,
            "envelope_stat_calls": counts[0].2,
            "peak_rss_setup_inclusive": hwm, "samples": 11, "cache": "warm tmpfs; no cold-cache control"})
        );
    }
}
