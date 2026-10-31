//! [ORB-13744] The bounded delivery observation a non-operator consumer may
//! read for one task-delivery run.
//!
//! `orbit.workflow.run.show` is the operator's whole-run view; this is the
//! narrow public one. Its evidence is the durable checkpoint of a host-run
//! deterministic step, found by the step id the job definition gives it and
//! cross-checked against the run's pipeline entry under that id. Agent step
//! outputs, response envelopes, prompts and step inputs are never read, so an
//! agent that writes a commit claim anywhere it can reach changes nothing here.
//!
//! Ownership fails closed before any evidence is read: the run must exist in
//! this workspace's run store, the task in its task store, the run's own
//! submitted `task_ids` must name the task, and the job must hold task
//! delivery. Evidence that is missing or does not match is reported as a typed
//! gap, never filled from a weaker source. A codebase scan observes tasks it
//! read from this workspace's own task listing, which stands in for the task
//! lookup. A claimed leaf's task lives in its owner's store instead, so its
//! executor answers for the leaf on the durable admission binding the leaf to
//! that task's claim.

mod checkpoint;
mod execution;
mod observation;
mod parsing;

pub(crate) use observation::CodebaseDeliveryScan;
