//! What escalation triage needs from distributed crew accounting
//! [ORB-17936].
//!
//! Triage passes over the crew that wrote a held pilot assessment, which
//! only the pilot run's recorded crew draws name. `distributed` owns that
//! reading, and it also drives task writes, so a direct call from here would
//! close a cycle. This port is the way in: `distributed` implements it on
//! [`OrbitRuntime`](crate::OrbitRuntime), so composing the runtime binds it.

use orbit_types::workflow::PipelineState;

/// The crew-accounting read task code calls, implemented by `distributed`.
pub(in crate::application) trait TaskDistributedPort {
    /// The crew a task-pilot run drew to write its assessments, from the
    /// run's `state`; `None` when the run recorded no pilot draw.
    fn pilot_assessment_crew(&self, state: &PipelineState) -> Option<String>;
}
