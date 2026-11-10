//! Immutable agent launch records from one scan, indexed by their job run.
//! A launch is recorded before its provider starts, so a run whose
//! invocation never completed still carries its dispatch effort here.

use std::collections::HashMap;

use orbit_store::contracts::AgentExecutionRecord;

/// Keep scan order for aggregate readers and narrow launch attribution to
/// its own run without repeatedly walking every workspace launch.
#[derive(Debug, Default)]
pub(in crate::application::agents) struct AgentLaunchFacts {
    records: Vec<AgentExecutionRecord>,
    by_run: HashMap<String, Vec<usize>>,
}

impl From<Vec<AgentExecutionRecord>> for AgentLaunchFacts {
    fn from(records: Vec<AgentExecutionRecord>) -> Self {
        let mut by_run: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, record) in records.iter().enumerate() {
            by_run.entry(record.run_id.clone()).or_default().push(index);
        }
        Self { records, by_run }
    }
}

impl AgentLaunchFacts {
    /// This run's launch records, in launch order. The records and index
    /// are built together and cannot be mutated apart.
    pub(in crate::application::agents) fn launches_for_run(
        &self,
        run_id: &str,
    ) -> impl Iterator<Item = &AgentExecutionRecord> + Clone {
        self.by_run
            .get(run_id)
            .into_iter()
            .flatten()
            .map(|&index| &self.records[index])
    }
}
