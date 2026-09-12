//! Worker-module tests.
//!
//! - `supervisor` — [`PipelineWorkerSupervisor`](super::supervisor::PipelineWorkerSupervisor)
//!   failure paths, driven against a store without an `OrbitRuntime`.
//!
//! Worker behavior that needs a composed runtime (spawning real child
//! processes, submission, routine dispatch) is exercised from
//! `application/tests/job_pipeline.rs`.

mod supervisor;
