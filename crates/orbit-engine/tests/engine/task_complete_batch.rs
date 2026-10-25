//! Batch `task_complete` must validate every task before completing any.
//!
//! Regression for ORB-17622: a `[review, proposed]` batch previously moved
//! the review task to `done` before refusing the proposed task, leaving a
//! partial completion behind a failed result.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::{TaskActivityUpdate, execute_deterministic_action};
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};
use serde_json::{Value, json};

const RUN_ID: &str = "task-complete-batch-run";

fn task(id: &str, status: TaskStatus) -> Task {
    let now = Utc::now();
    Task {
        requires_labels: Vec::new(),
        job_run_machine: None,
        id: id.to_string(),
        title: format!("Task {id}"),
        description: String::new(),
        acceptance_criteria: Vec::new(),
        tags: Vec::new(),
        required_tools: Vec::new(),
        plan: String::new(),
        execution_summary: String::new(),
        context_files: Vec::new(),
        created_by: None,
        planned_by: None,
        implemented_by: None,
        status,
        priority: TaskPriority::Medium,
        complexity: None,
        task_type: TaskType::Chore,
        pr_status: None,
        external_refs: Vec::new(),
        relations: Vec::new(),
        job_run_id: Some(RUN_ID.to_string()),
        crew: None,
        crew_source: None,
        orchestrator: None,
        orchestrator_model: None,
        orchestrator_model_source: None,
        origin: None,
        created_at: now,
        updated_at: now,
    }
}

struct BatchHost {
    tasks: Mutex<BTreeMap<String, Task>>,
    owed: Mutex<BTreeMap<String, Vec<usize>>>,
}

impl BatchHost {
    fn new(tasks: Vec<Task>) -> Self {
        Self {
            tasks: Mutex::new(
                tasks
                    .into_iter()
                    .map(|task| (task.id.clone(), task))
                    .collect(),
            ),
            owed: Mutex::new(BTreeMap::new()),
        }
    }

    fn with_owed(tasks: Vec<Task>, owed: &[(&str, Vec<usize>)]) -> Self {
        let host = Self::new(tasks);
        *host.owed.lock().expect("owed") = owed
            .iter()
            .map(|(id, criteria)| (id.to_string(), criteria.clone()))
            .collect();
        host
    }

    fn status(&self, id: &str) -> TaskStatus {
        self.tasks.lock().expect("tasks")[id].status
    }
}

impl orbit_engine::test_support::NullRuntimeHost for BatchHost {
    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        self.tasks
            .lock()
            .expect("tasks")
            .get(task_id)
            .cloned()
            .ok_or_else(|| OrbitError::Execution(format!("unknown task '{task_id}'")))
    }

    fn update_task_from_activity(
        &self,
        task_id: &str,
        update: TaskActivityUpdate,
    ) -> Result<Task, OrbitError> {
        let mut tasks = self.tasks.lock().expect("tasks");
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| OrbitError::Execution(format!("unknown task '{task_id}'")))?;
        if task.status != update.expected_status {
            return Err(OrbitError::Execution(format!(
                "task '{task_id}' status changed from {} to {}",
                update.expected_status, task.status
            )));
        }
        task.status = update.status;
        Ok(task.clone())
    }

    fn outstanding_owed_criteria(
        &self,
        task_id: &str,
        _run_id: Option<&str>,
    ) -> Result<Vec<usize>, OrbitError> {
        Ok(self
            .owed
            .lock()
            .expect("owed")
            .get(task_id)
            .cloned()
            .unwrap_or_default())
    }
}

fn complete(host: &BatchHost, task_ids: &[&str]) -> Result<Value, OrbitError> {
    execute_deterministic_action(
        host,
        "task_complete",
        &json!({}),
        &json!({
            "run_id": RUN_ID,
            "task_ids": task_ids,
        }),
        false,
        &HashMap::new(),
        None,
    )
}

#[test]
fn a_batch_with_a_non_review_task_fails_without_changing_any_status() {
    let host = BatchHost::new(vec![
        task("T-review", TaskStatus::Review),
        task("T-proposed", TaskStatus::Proposed),
    ]);

    let error = complete(&host, &["T-review", "T-proposed"])
        .expect_err("a proposed task must refuse the whole batch");

    assert!(
        error.to_string().contains("T-proposed"),
        "the refusal names the non-review task: {error}"
    );
    assert_eq!(
        host.status("T-review"),
        TaskStatus::Review,
        "the first task must remain in review when a later task refuses the batch"
    );
    assert_eq!(host.status("T-proposed"), TaskStatus::Proposed);
}

#[test]
fn a_valid_all_review_batch_completes_every_task() {
    let host = BatchHost::new(vec![
        task("T-one", TaskStatus::Review),
        task("T-two", TaskStatus::Review),
    ]);

    let output = complete(&host, &["T-one", "T-two"]).expect("all-review batch completes");

    assert_eq!(output["completed_task_ids"], json!(["T-one", "T-two"]));
    assert_eq!(host.status("T-one"), TaskStatus::Done);
    assert_eq!(host.status("T-two"), TaskStatus::Done);
}

#[test]
fn a_batch_with_an_owed_review_task_completes_only_the_unowed_tasks() {
    let host = BatchHost::with_owed(
        vec![
            task("T-owed", TaskStatus::Review),
            task("T-ready", TaskStatus::Review),
        ],
        &[("T-owed", vec![0])],
    );

    let output = complete(&host, &["T-owed", "T-ready"]).expect("owed tasks are skipped");

    assert_eq!(output["completed_task_ids"], json!(["T-ready"]));
    assert_eq!(output["owed_task_ids"], json!(["T-owed"]));
    assert_eq!(host.status("T-owed"), TaskStatus::Review);
    assert_eq!(host.status("T-ready"), TaskStatus::Done);
}
