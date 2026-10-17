use chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRunOutcomeFact {
    /// Run identifier. Reliability counting keys a wrapper echo by this id
    /// only when the run recorded no leaf of its own.
    pub run_id: String,
    pub job_id: String,
    pub state: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityInvocationCount {
    pub activity_id: String,
    pub invocation_count: u64,
    pub job_run_count: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InvocationRunCoverage {
    pub total_job_runs: u64,
    pub matching_job_runs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedFacts<T> {
    pub facts: Vec<T>,
    pub truncated: bool,
}
