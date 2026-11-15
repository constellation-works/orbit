//! Built-in deterministic automation actions for the Orbit engine.
//!
//! [`automation::execute_action`] parses an engine action name and runs that
//! action. v2 job steps run the same actions through `activity_job`'s dispatcher.

pub mod automation;
