//! Job-run records through the runtime's `JobRuns` operations over a bare
//! store: admission, lifecycle, checkpoint state, local pull occupancy, and
//! the retention and resource-usage reads that see finished runs.

mod automation_admission;
mod pull_admission;
mod resource_usage;
mod retention;
mod states;
