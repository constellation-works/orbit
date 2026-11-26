use super::{Command, Operation};
use crate::metrics::cost::{ManagedExecutionCost, MetricsWindow};
use crate::metrics::reliability::{JobDefinitionReads, ReliabilityWindow, RuntimeReliability};

impl<'a> Command<'a, ManagedExecutionCost> {
    /// Read execution costs in the given window.
    pub fn cost(window: MetricsWindow) -> Self {
        Self::new(Operation::command("run", "cost"), move |core| {
            core.runtime().managed_execution_cost(window)
        })
    }
}

impl<'a> Command<'a, Vec<Result<RuntimeReliability, crate::OrbitError>>> {
    /// Read each workspace in the submitted order, sharing job definition reads across the fleet.
    pub fn fleet_reliability(window: &'a ReliabilityWindow, others: &'a [&'a crate::Core]) -> Self {
        Self::new(Operation::command("run", "reliability"), move |first| {
            let definitions = JobDefinitionReads::default();
            Ok(super::fleet::read(first, others, |runtime| {
                runtime.pipeline_reliability_sharing_reads(window, &definitions)
            }))
        })
    }
}
