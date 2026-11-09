//! Immutable invocation records from one scan, indexed by their job run.

use std::collections::HashMap;

use orbit_store::contracts::InvocationAccountingFact;

/// Keep scan order for aggregate readers and narrow unit attribution to
/// its own run without repeatedly walking every workspace invocation.
#[derive(Debug, Default)]
pub(in crate::application::agents) struct AgentInvocationFacts {
    records: Vec<InvocationAccountingFact>,
    by_run: HashMap<String, Vec<usize>>,
}

impl From<Vec<InvocationAccountingFact>> for AgentInvocationFacts {
    fn from(records: Vec<InvocationAccountingFact>) -> Self {
        let mut by_run: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, record) in records.iter().enumerate() {
            by_run
                .entry(record.job_run_id.clone())
                .or_default()
                .push(index);
        }
        Self { records, by_run }
    }
}

impl AgentInvocationFacts {
    /// All records in the order returned by the store.
    pub(in crate::application::agents) fn iter_invocations(
        &self,
    ) -> std::slice::Iter<'_, InvocationAccountingFact> {
        self.records.iter()
    }

    /// This run's records, retaining their original relative order. The
    /// records and index are built together and cannot be mutated apart.
    pub(in crate::application::agents) fn invocations_for_run(
        &self,
        run_id: &str,
    ) -> impl Iterator<Item = &InvocationAccountingFact> + Clone {
        self.by_run
            .get(run_id)
            .into_iter()
            .flatten()
            .map(|&index| &self.records[index])
    }
}
