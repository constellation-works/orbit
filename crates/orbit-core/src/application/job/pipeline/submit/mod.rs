//! Ship, catalog pipeline and auto-drain receipt submission.

mod auto_drain_receipt;
mod pipeline_run;
mod ship;

pub use auto_drain_receipt::{
    AutoDrainRequest, AutoDrainSubmissionProfile, AutoDrainSubmissionReceipt,
    AutoDrainSubmissionSource,
};
pub(crate) use pipeline_run::{
    ChildPipelineAdmission, ChildSubmission, PipelineSubmission, SubmittedDefinition,
};
pub use ship::{ShipModeResolutionProfile, ShipRunRequest};
