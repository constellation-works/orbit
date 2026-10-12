//! Readiness and the drain's backlog snapshot select open tasks from the
//! generated index, so closed history costs them no envelope reads, while
//! the closed tasks their rules still reach stay visible: an epic root's
//! closed descendants, and a closed task an explicit request names.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, TaskComplexity, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use serde_json::{Value, json};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

use super::dispatch_admission::{PressureProbe, isolated};

/// Closed tasks in the workspace: far more than a bounded selection reads.
const CLOSED: usize = 300;

/// Counts `task.yaml` parses: the store opens one span per envelope read.
struct EnvelopeReads(Arc<AtomicU64>);

impl<S: tracing::Subscriber> Layer<S> for EnvelopeReads {
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        _: &tracing::span::Id,
        _: Context<'_, S>,
    ) {
        if attrs.metadata().name() == "task_envelope_read" {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn add(
    runtime: &OrbitRuntime,
    title: &str,
    status: TaskStatus,
    parent: Option<&str>,
    tags: &[&str],
    context_files: &[&str],
) -> String {
    runtime
        .add_task(TaskAddParams {
            title: title.into(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: vec!["The task is observable.".into()],
            plan: "Fixture plan.".into(),
            complexity: TaskComplexity::Medium,
            parent_id: parent.map(|id| id.parse().unwrap()),
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            context_files: context_files
                .iter()
                .map(|file| (*file).to_string())
                .collect(),
            task_type: Some(orbit_core::TaskType::Chore),
            status: Some(status),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string()
}

fn readiness(runtime: &OrbitRuntime, task_ids: &[String]) -> Result<Value, orbit_core::OrbitError> {
    runtime.workspace_auto_readiness(task_ids, None, 50, &[])
}

fn entry<'a>(readiness: &'a Value, task: &str) -> &'a Value {
    readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == task)
        .unwrap_or_else(|| panic!("{task} missing from readiness: {readiness}"))
}

#[test]
fn readiness_reads_open_work_and_the_closed_tasks_its_rules_reach() {
    if !isolated(
        "readiness_population::readiness_reads_open_work_and_the_closed_tasks_its_rules_reach",
    ) {
        return;
    }
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    for index in 0..CLOSED {
        add(
            &runtime,
            &format!("closed {index}"),
            TaskStatus::Done,
            None,
            &[],
            &["file:README.md"],
        );
    }
    let ready = add(
        &runtime,
        "ready",
        TaskStatus::Backlog,
        None,
        &[],
        &["file:src/ready.rs"],
    );
    // An epic root that declared nothing while its (closed) child did: it
    // inherited its whole footprint, so admission withholds it.
    let root_task = add(
        &runtime,
        "epic root",
        TaskStatus::Backlog,
        None,
        &["epic"],
        &[],
    );
    add(
        &runtime,
        "closed child",
        TaskStatus::Done,
        Some(&root_task),
        &[],
        &["file:src/child.rs"],
    );
    let closed = add(
        &runtime,
        "closed explicit",
        TaskStatus::Done,
        None,
        &[],
        &["file:README.md"],
    );
    // Two backlog siblings under a closed parent form one group: a lock
    // conflict on one holds the other, which only the parent links.
    add(
        &runtime,
        "holder",
        TaskStatus::InProgress,
        None,
        &[],
        &["file:src/held.rs"],
    );
    let group = add(
        &runtime,
        "closed parent",
        TaskStatus::Done,
        None,
        &[],
        &["file:README.md"],
    );
    let blocked = add(
        &runtime,
        "blocked",
        TaskStatus::Backlog,
        Some(&group),
        &[],
        &["file:src/held.rs"],
    );
    let sibling = add(
        &runtime,
        "sibling",
        TaskStatus::Backlog,
        Some(&group),
        &[],
        &["file:src/sibling.rs"],
    );
    // One listing records the envelope stamps a later process accepts.
    runtime.list_tasks().unwrap();
    drop(runtime);

    let runtime = OrbitRuntime::from_roots(&global, &workspace)
        .unwrap()
        .with_host_resource_probe(PressureProbe::calm());
    let reads = Arc::new(AtomicU64::new(0));
    let subscriber = tracing_subscriber::registry().with(EnvelopeReads(Arc::clone(&reads)));
    let report =
        tracing::subscriber::with_default(subscriber, || readiness(&runtime, &[]).unwrap());
    let reads = reads.load(Ordering::Relaxed);
    assert!(
        reads <= 100,
        "readiness over {CLOSED} closed tasks must read only open work and what its rules reach: read {reads}"
    );
    assert_eq!(report["total"], 4, "{report}");
    assert_eq!(
        entry(&report, &blocked)["reason"],
        "context_lock_conflict",
        "{report}"
    );
    assert_eq!(
        entry(&report, &sibling)["reason"],
        "group_member_conflict",
        "{report}"
    );
    assert_eq!(entry(&report, &ready)["eligible"], true, "{report}");
    let withheld = entry(&report, &root_task);
    assert_eq!(withheld["eligible"], false, "{report}");
    assert_eq!(withheld["reason"], "inherited_only_epic_root", "{report}");

    let backlog = runtime
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &json!({}),
            ToolContext::default(),
        )
        .unwrap();
    assert!(
        backlog["excluded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == root_task.as_str()
                && entry["reason"] == "inherited_only_epic_root"),
        "{backlog}"
    );

    let explicit = readiness(&runtime, std::slice::from_ref(&closed)).unwrap();
    let named = entry(&explicit, &closed);
    assert_eq!(named["status"], "done", "{explicit}");
    assert_eq!(named["reason"], "not_backlog", "{explicit}");
    for unknown in ["ORB-999999", "not-a-task"] {
        let error = readiness(&runtime, &[unknown.to_string()]).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("was not found in this workspace"),
            "{unknown}: {error}"
        );
    }
}

#[test]
fn restored_closed_child_retains_inherited_epic_footprint() {
    if !isolated("readiness_population::restored_closed_child_retains_inherited_epic_footprint") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let previous = add(
        &runtime,
        "previous parent",
        TaskStatus::Done,
        None,
        &[],
        &[],
    );
    let epic = add(&runtime, "epic", TaskStatus::Backlog, None, &["epic"], &[]);
    let child = add(
        &runtime,
        "closed descendant",
        TaskStatus::Done,
        Some(&previous),
        &[],
        &["file:src/child.rs"],
    );
    runtime.list_tasks().unwrap();
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(&global),
    )
    .unwrap();
    let path = registry
        .canonical_task_bundle_path(&runtime.workspace_id().unwrap(), &child)
        .unwrap()
        .join(orbit_types::task::TASK_ENVELOPE_FILE_NAME);
    let mut envelope: orbit_types::task::TaskEnvelopeV2 =
        serde_yaml::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for relation in &mut envelope.relations {
        if relation.relation_type == orbit_types::task::TaskRelationType::ChildOf {
            relation.target = epic.clone();
        }
    }
    let restored = path.with_extension("restored");
    std::fs::write(&restored, serde_yaml::to_string(&envelope).unwrap()).unwrap();
    std::fs::rename(restored, path).unwrap();
    drop(runtime);

    let runtime = OrbitRuntime::from_roots(&global, &workspace)
        .unwrap()
        .with_host_resource_probe(PressureProbe::calm());
    let report = readiness(&runtime, std::slice::from_ref(&epic)).unwrap();
    assert_eq!(entry(&report, &epic)["eligible"], false, "{report}");
    assert_eq!(
        entry(&report, &epic)["reason"],
        "inherited_only_epic_root",
        "{report}"
    );
    let backlog = runtime
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &json!({}),
            ToolContext::default(),
        )
        .unwrap();
    assert!(
        backlog["excluded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == epic && entry["reason"] == "inherited_only_epic_root"),
        "{backlog}"
    );
}
