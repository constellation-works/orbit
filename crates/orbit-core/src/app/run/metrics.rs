use super::{Command, Operation};
use crate::application::job::JobRunListParams;
use crate::metric::aggregate;
use crate::metric::reliability::JobDefinitionReads;

pub use crate::metric::cost::{
    ManagedExecutionCost, MetricsWindow, NormalizedTokenSummary, OrchestrationBucketKind,
    OrchestrationBucketSummary, OrchestrationModelSummary, OrchestrationSummary, TaskTokenSpend,
    TaskTokenTotals,
};
pub use crate::metric::reliability::{
    ActivityCount, ActivityRole, BucketGranularity, BucketOutcomeRow, JobOutcomeRow,
    JobRunReliability, MIN_CONFIDENT_SAMPLE, OutcomeCounts, Rate, RecoveryReliability,
    ReliabilityWindow, RuntimeReliability,
};
pub use crate::metric::{
    DoubleReadSummary, KnowledgeStatsSummary, RatioSummary, TokenInputSummary,
};

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

impl<'a> Command<'a, KnowledgeStatsSummary> {
    /// List up to `limit` recent job runs and aggregate their historical knowledge metrics.
    pub fn knowledge_metrics(limit: usize) -> Self {
        Self::new(
            Operation::dashboard_read("run", "knowledge-metrics"),
            move |core| {
                let runs = core.runtime().list_job_runs(JobRunListParams {
                    limit: Some(limit),
                    ..Default::default()
                })?;
                Ok(aggregate(&runs))
            },
        )
    }
}
