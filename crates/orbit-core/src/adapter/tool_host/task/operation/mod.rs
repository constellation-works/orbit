//! Handlers for task verbs declared in the task operation registry.

mod dispatch;
mod lifecycle;
mod listing;
mod show;

pub(in crate::adapter::tool_host) use dispatch::dispatch;
