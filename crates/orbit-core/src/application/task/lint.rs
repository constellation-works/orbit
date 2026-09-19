use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use orbit_common::OrbitError;
use orbit_common::fs::selector::anchor_path;
use orbit_types::task::Task;
use serde::{Deserialize, Serialize};

use crate::OrbitRuntime;
use crate::application::task::DeclaredContextFiles;

use super::paths::{context_workspace_root, extract_task_path_mentions, task_path_exists};
use crate::runtime::task::declared_context_files;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskLintReport {
    pub task_id: orbit_types::identity::OrbitId,
    pub duration_ms: u64,
    pub finding_count: usize,
    pub findings: Vec<TaskLintFinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskLintFinding {
    pub severity: TaskLintSeverity,
    pub check: String,
    pub message: String,
    pub fix_it: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskLintSeverity {
    Error,
    Warning,
}

impl OrbitRuntime {
    pub fn lint_task(&self, id: &str) -> Result<TaskLintReport, OrbitError> {
        let started_at = Instant::now();
        let task = self.get_task(id)?;
        let workspace_root = context_workspace_root(&self.paths().repo_root, None);
        let declared = declared_context_files(&task.context_files, &workspace_root);
        let description_paths = extract_task_path_mentions(&task.description);
        let mut findings = Vec::new();

        self.lint_context_surface(&task, &declared, &mut findings)?;
        lint_context_file_paths(&declared, &workspace_root, &mut findings);
        lint_description_paths(&description_paths, &workspace_root, &mut findings);
        lint_context_completeness(
            &declared.retained,
            &description_paths,
            &workspace_root,
            &mut findings,
        );
        lint_acceptance_criteria(&task.acceptance_criteria, &mut findings);

        Ok(TaskLintReport {
            task_id: task.id,
            duration_ms: started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            finding_count: findings.len(),
            findings,
        })
    }
}

impl OrbitRuntime {
    /// Report the declarations that leave the task without a usable lock
    /// surface.
    ///
    /// A task that declares nothing has no lock surface to reserve, and one
    /// whose every selector is unusable is the same refusal with a different
    /// remedy. The legacy v2 dispatch admission path permits an empty surface,
    /// but an operator task-scope reservation refuses it and distributed pull
    /// admission will exclude it. Pruning history, when it exists, names
    /// exactly what the task used to declare, so the diagnostic points at the
    /// evidence-backed repair rather than asking for a guess ([ORB-12490]).
    fn lint_context_surface(
        &self,
        task: &Task,
        declared: &DeclaredContextFiles,
        findings: &mut Vec<TaskLintFinding>,
    ) -> Result<(), OrbitError> {
        for selector in &declared.invalid {
            findings.push(TaskLintFinding {
                severity: TaskLintSeverity::Error,
                check: "path_validity".to_string(),
                message: format!(
                    "context selector `{selector}` is not a valid in-repository selector"
                ),
                fix_it: format!(
                    "Replace `{selector}` through `orbit task update --context` with a canonical `file:`, `dir:`, or `symbol:` selector inside the repository."
                ),
            });
        }

        if !declared.retained.is_empty() {
            return Ok(());
        }
        // This is advisory: legacy v2 admission permits an empty surface, so
        // no task type is blocked by this lint finding. The operator
        // reservation and distributed pull paths still need a real surface.
        let severity = TaskLintSeverity::Warning;

        // Only an empty surface needs the history read, so the sweep over
        // every active task does not load history it will not use.
        let restoration = self.plan_context_file_restore(task.id.as_str())?;
        let remedy = if restoration.restored.is_empty() {
            "Declare the files this task will modify with `orbit task update --context` before claiming an operator task-scope reservation or entering distributed pull admission; legacy v2 admission currently permits an empty surface.".to_string()
        } else {
            format!(
                "Task history records {} previously pruned selector(s); restore them with `orbit task lint {} --restore-pruned`, or declare the scope with `orbit task update --context` before claiming an operator task-scope reservation or entering distributed pull admission.",
                restoration.restored.len(),
                task.id
            )
        };
        findings.push(TaskLintFinding {
            severity,
            check: "context_surface".to_string(),
            message: "task declares no usable `context_files`; legacy v2 admission permits an empty surface, but operator task-scope reservation refuses it and distributed pull admission will exclude it".to_string(),
            fix_it: remedy,
        });
        Ok(())
    }
}

/// Report declared targets that do not exist in the checkout.
///
/// This is a warning, not an error, and never advises removing the selector: a
/// declaration for a file the task is about to create is valid, holds its lock
/// before creation, and is exactly what filesystem-existence pruning used to
/// destroy ([ORB-12490]). What the finding buys is a typo check.
fn lint_context_file_paths(
    declared: &DeclaredContextFiles,
    workspace_root: &Path,
    findings: &mut Vec<TaskLintFinding>,
) {
    for path in &declared.retained {
        if task_path_exists(workspace_root, path) {
            continue;
        }
        findings.push(TaskLintFinding {
            severity: TaskLintSeverity::Warning,
            check: "context_target_missing".to_string(),
            message: format!(
                "context file `{path}` does not exist in the task worktree; the declaration is kept and keeps holding its lock"
            ),
            fix_it: format!(
                "Confirm the task creates `{path}`. If it is a typo, correct it with `orbit task update --context`; a not-yet-created target needs no change."
            ),
        });
    }
}

fn lint_description_paths(
    mentioned_paths: &[String],
    workspace_root: &Path,
    findings: &mut Vec<TaskLintFinding>,
) {
    for path in mentioned_paths {
        if task_path_exists(workspace_root, path) {
            continue;
        }
        findings.push(TaskLintFinding {
            severity: TaskLintSeverity::Error,
            check: "path_validity".to_string(),
            message: format!("description references `{path}`, but that path does not exist"),
            fix_it: format!(
                "Update the task description to reference an existing file, or add `{path}` to the worktree."
            ),
        });
    }
}

fn lint_context_completeness(
    context_files: &[String],
    mentioned_paths: &[String],
    workspace_root: &Path,
    findings: &mut Vec<TaskLintFinding>,
) {
    let known_context: BTreeSet<&str> = context_files.iter().map(String::as_str).collect();
    for path in mentioned_paths {
        if !task_path_exists(workspace_root, path)
            || known_context.contains(path.as_str())
            || context_files
                .iter()
                .any(|entry| context_entry_covers_path(entry, path))
        {
            continue;
        }
        findings.push(TaskLintFinding {
            severity: TaskLintSeverity::Warning,
            check: "context_completeness".to_string(),
            message: format!(
                "description references `{path}`, but it is missing from `context_files`"
            ),
            fix_it: format!("Add `{path}` to `context_files` so implementers get the right scope."),
        });
    }
}

fn lint_acceptance_criteria(acceptance_criteria: &[String], findings: &mut Vec<TaskLintFinding>) {
    const GENERIC_PHRASES: &[&str] = &[
        "implement the feature",
        "implement feature",
        "fix the bug",
        "fix bug",
        "make it work",
        "ensure it works",
        "support the change",
        "handle edge cases",
        "works correctly",
        "update as needed",
    ];
    const NON_DETERMINISTIC_TERMS: &[&str] = &[
        "appropriately",
        "reasonable",
        "clean",
        "intuitive",
        "user-friendly",
        "robust",
        "better",
        "improved",
        "as needed",
        "if needed",
    ];

    for criterion in acceptance_criteria {
        let trimmed = criterion.trim();
        if trimmed.is_empty() {
            findings.push(TaskLintFinding {
                severity: TaskLintSeverity::Warning,
                check: "ac_specificity".to_string(),
                message: "acceptance criterion is blank".to_string(),
                fix_it: "Replace blank acceptance criteria with observable outcomes.".to_string(),
            });
            continue;
        }

        let normalized = trimmed.to_lowercase();
        let has_observable_detail = trimmed.contains('`')
            || trimmed.contains('/')
            || trimmed.chars().any(|ch| ch.is_ascii_digit())
            || [
                "json", "warning", "error", "path", "status", "output", "under ",
            ]
            .iter()
            .any(|needle| normalized.contains(needle));
        let is_generic = GENERIC_PHRASES.iter().any(|phrase| normalized == *phrase);
        let is_too_short = trimmed.len() < 20;
        let is_non_deterministic = NON_DETERMINISTIC_TERMS
            .iter()
            .any(|term| normalized.contains(term));

        if is_too_short || is_generic || (is_non_deterministic && !has_observable_detail) {
            findings.push(TaskLintFinding {
                severity: TaskLintSeverity::Warning,
                check: "ac_specificity".to_string(),
                message: format!(
                    "acceptance criterion is too broad or non-deterministic: `{trimmed}`"
                ),
                fix_it: "Rewrite the criterion as an observable outcome: name the command, file, output, error, or measurable threshold.".to_string(),
            });
        }
    }
}

fn context_entry_covers_path(entry: &str, mentioned_path: &str) -> bool {
    let Ok(entry_anchor) = anchor_path(entry) else {
        return false;
    };
    let Ok(mentioned_anchor) = anchor_path(mentioned_path) else {
        return false;
    };
    let entry_anchor = entry_anchor.to_string_lossy().replace('\\', "/");
    let mentioned_anchor = mentioned_anchor.to_string_lossy().replace('\\', "/");
    entry_anchor == mentioned_anchor
        || entry_anchor
            .strip_prefix(format!("{mentioned_anchor}/").as_str())
            .is_some()
        || mentioned_anchor
            .strip_prefix(format!("{entry_anchor}/").as_str())
            .is_some()
}

#[cfg(test)]
mod tests {
    use super::context_entry_covers_path;

    #[test]
    fn context_entry_covers_file_line_mentions() {
        assert!(context_entry_covers_path(
            "file:crates/orbit-cli/src/command/ship.rs",
            "crates/orbit-cli/src/command/ship.rs:274"
        ));
        assert!(context_entry_covers_path(
            "symbol:crates/x.rs#run:function",
            "crates/x.rs:42"
        ));
        assert!(context_entry_covers_path("dir:src", "src/lib.rs"));
        assert!(!context_entry_covers_path(
            "file:src/lib.rs",
            "tests/lib.rs"
        ));
    }
}
