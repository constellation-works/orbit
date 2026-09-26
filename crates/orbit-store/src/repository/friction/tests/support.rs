//! Shared fixtures for the friction store tests.

use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use orbit_types::record::{FrictionRecord, FrictionStatus};
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};

use crate::Store;
use crate::driver::file::friction_store::write_record_at;
use crate::repository::friction::{FrictionAddParams, FrictionStore};

/// A file-backed store, so tests exercise the same read-pool path production
/// uses rather than the in-memory writer fallback.
pub(super) fn store(root: &Path) -> Store {
    Store::open(&root.join("orbit.db")).expect("open store")
}

pub(super) fn friction_store(root: &Path, workspace_id: &str) -> FrictionStore {
    let store = store(root);
    let files_root = root.join(workspace_id);
    crate::workflow::friction::import_workspace_frictions(&store, workspace_id, &files_root)
        .expect("import legacy friction records");
    FrictionStore::open(store, workspace_id, files_root).expect("open friction store")
}

pub(super) fn add_params(
    model: &str,
    created_at: DateTime<Utc>,
    tags: &[&str],
) -> FrictionAddParams {
    FrictionAddParams {
        model: model.to_string(),
        title: None,
        body: "Body".to_string(),
        tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
        during_task: None,
        created_at,
    }
}

pub(super) fn at(day: u32, hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, day, hour, 0, 0)
        .single()
        .expect("fixture timestamp")
}

/// Write one legacy Markdown record into `root` at the location its ID
/// addresses.
pub(super) fn legacy_record(root: &Path, id: &str, model: &str, status: FrictionStatus) {
    let record = legacy_body(id, model, status);
    let month = &id[1..8];
    let seq = &id[9..12];
    write_record_at(&root.join(month).join(format!("F{seq}.md")), &record).expect("legacy record");
}

pub(super) fn legacy_body(id: &str, model: &str, status: FrictionStatus) -> FrictionRecord {
    FrictionRecord {
        id: id.to_string(),
        title: Some(format!("Handle for {id}")),
        model: model.to_string(),
        created_at: at(10, 12),
        status,
        tags: vec!["tooling".to_string()],
        resolved_at: matches!(status, FrictionStatus::Resolved).then(|| at(11, 9)),
        during_task: Some("ORB-00001".to_string()),
        resolved_by_task: matches!(status, FrictionStatus::Resolved)
            .then(|| "ORB-00002".to_string()),
        rehome_to: None,
        body: format!("Report body for {id}"),
    }
}

pub(super) fn done_task(id: &str, implemented_by: &str) -> Task {
    Task {
        job_run_machine: None,
        id: id.to_string(),
        title: id.to_string(),
        description: String::new(),
        acceptance_criteria: Vec::new(),
        tags: Vec::new(),
        required_tools: Vec::new(),
        plan: String::new(),
        execution_summary: String::new(),
        context_files: Vec::new(),
        created_by: None,
        planned_by: None,
        implemented_by: Some(implemented_by.to_string()),
        status: TaskStatus::Done,
        priority: TaskPriority::Medium,
        complexity: None,
        task_type: TaskType::Chore,
        pr_status: None,
        external_refs: Vec::new(),
        relations: Vec::new(),
        job_run_id: None,
        crew: None,
        orchestrator: None,
        created_at: at(1, 0),
        updated_at: at(1, 0),
    }
}
