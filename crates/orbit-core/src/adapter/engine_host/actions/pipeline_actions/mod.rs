//! Deterministic actions that dispatch child Jobs for pipeline sequencers and
//! wait on them. The decisions they consult between waits (gate admission)
//! and the other pipeline actions live in `application::job::pipeline_actions`.

mod invoke;

#[cfg(test)]
mod tests;

pub(super) use invoke::{invoke_and_wait, invoke_detached};
