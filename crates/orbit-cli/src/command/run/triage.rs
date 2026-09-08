//! `orbit run triage` CLI entrypoint [ORB-10129].

use clap::Args;
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute};

use super::support::{dispatch_workflow, workflow_dispatch_payload};

pub(super) const TRIAGE_WORKFLOW: &str = "triage";

#[derive(Args)]
#[command(
    about = "Triage tasks blocked by failed runs; re-backlog environmental failures",
    override_usage = "orbit run triage [<TASK_ID>...] [OPTIONS]",
    after_help = "Examples:\n  orbit run triage\n  orbit run triage <TASK_ID>\n\nOnly blocked tasks coupled to a failed job run are triaged; tasks a human\nblocked by hand are never touched. An empty candidate set is a clean no-op.\nInspect submitted runs with `orbit run history -j task_triage_pipeline` and\n`orbit run show <RUN_ID>`."
)]
pub struct TriageCommand {
    /// Optional task IDs to narrow the triage scan. Omit to scan every
    /// blocked task attributable to a failed run.
    #[arg(value_name = "TASK_ID", num_args = 0..)]
    pub task_ids: Vec<String>,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

impl Execute for TriageCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let input = build_triage_input(&self.task_ids)?;
        let runs = dispatch_workflow(runtime, TRIAGE_WORKFLOW, &input, false, false, 1)?;
        workflow_dispatch_payload(TRIAGE_WORKFLOW, &runs)
    }
}

pub(crate) fn build_triage_input(task_ids: &[String]) -> Result<Value, OrbitError> {
    let mut seen = std::collections::HashSet::new();
    for task_id in task_ids {
        if task_id.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task id in explicit triage selection must not be empty".to_string(),
            ));
        }
        if !seen.insert(task_id.as_str()) {
            return Err(OrbitError::InvalidInput(format!(
                "duplicate task id '{task_id}' in explicit triage selection"
            )));
        }
    }
    if task_ids.is_empty() {
        Ok(json!({}))
    } else {
        Ok(json!({ "task_ids": task_ids }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_triage_input_omits_task_ids_when_empty() {
        let input = build_triage_input(&[]).expect("input builds");
        assert!(input.get("task_ids").is_none());
    }

    #[test]
    fn build_triage_input_passes_explicit_ids_and_rejects_bad_selections() {
        let ids = vec!["ORB-1".to_string(), "ORB-2".to_string()];
        let input = build_triage_input(&ids).expect("input builds");
        assert_eq!(input["task_ids"], json!(["ORB-1", "ORB-2"]));

        let dup = vec!["ORB-1".to_string(), "ORB-1".to_string()];
        assert!(build_triage_input(&dup).is_err());
        let blank = vec!["  ".to_string()];
        assert!(build_triage_input(&blank).is_err());
    }
}
