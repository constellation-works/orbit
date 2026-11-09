use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::super::{AgentFacts, ExecutionMode, MODEL_UNKNOWN, WindowBounds};

/// Managed, interactive and unclassified task counts.
#[derive(Clone, Default)]
pub(super) struct ExecutionCounts {
    managed: usize,
    interactive: usize,
    unclassified: usize,
}

impl ExecutionCounts {
    fn add(&mut self, mode: ExecutionMode) {
        match mode {
            ExecutionMode::Managed => self.managed += 1,
            ExecutionMode::Interactive => self.interactive += 1,
            ExecutionMode::Unclassified => self.unclassified += 1,
        }
    }
}

pub(super) fn execution_json(counts: &ExecutionCounts) -> Value {
    json!({
        "managed_tasks": counts.managed,
        "interactive_tasks": counts.interactive,
        "unclassified_tasks": counts.unclassified,
    })
}

/// Window-wide, and not filtered by the ranking's execution selector.
pub(super) fn execution_census(
    facts: &AgentFacts,
    window: WindowBounds,
) -> (ExecutionCounts, BTreeMap<String, ExecutionCounts>) {
    let mut board = ExecutionCounts::default();
    let mut by_model: BTreeMap<String, ExecutionCounts> = BTreeMap::new();
    for task in &facts.tasks {
        let mut models = BTreeSet::new();
        let mut counted = false;
        for attempt in &task.attempts {
            if !window.admits(attempt.start) || attempt.no_diff() {
                continue;
            }
            counted = true;
            if let Some(model) = attempt.runner.model.as_deref()
                && !model.is_empty()
                && model != MODEL_UNKNOWN
            {
                models.insert(model.to_string());
            }
        }
        if !counted {
            continue;
        }
        board.add(task.execution);
        for model in models {
            by_model.entry(model).or_default().add(task.execution);
        }
    }
    (board, by_model)
}
