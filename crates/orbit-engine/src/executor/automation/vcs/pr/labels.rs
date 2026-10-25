//! Task metadata mirrored to forge labels after any successful PR publication.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::handoff::HandoffContext;
use super::super::operations;

pub(super) fn apply_task_labels<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffContext,
    output: &mut Value,
) {
    let conventions = host.pr_config().conventions;
    let mut labels = BTreeSet::new();
    for task in &context.tasks {
        labels.extend(
            task.tags
                .iter()
                .filter(|tag| conventions.task_tag_labels.contains(tag))
                .cloned(),
        );
        if let Some(label) = conventions
            .task_type_labels
            .get(&task.task_type.to_string())
        {
            labels.insert(label.clone());
        }
    }
    if labels.is_empty() {
        return;
    }
    output["task_labels"] = match host.run_private_vcs_operation(
        operations::PR_ADD_LABELS,
        json!({
            "pr": output["pr_number"],
            "workspace_path": context.workspace_path,
            "labels": labels,
        }),
    ) {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!(%error, "pr_open could not apply task-derived labels");
            json!({
                "applied": [],
                "failed": labels.into_iter().map(|label| json!({
                    "label": label, "reason": error.to_string(),
                })).collect::<Vec<_>>(),
            })
        }
    };
}
