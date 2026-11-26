//! The decision-request CLI, derived from the decision operation registry.
//!
//! Every verb runs through the task app's one `exec`, which authorizes and
//! audits it. `list` and `show` reach the read-only `orbit.decision.*`
//! tools. `answer` is the operator's and never goes through the tool
//! surface: core applies the answer as this CLI's human writer. That is why
//! an agent session can list a request it is waiting on but cannot answer
//! it. `submit` goes through the governed `orbit.decision.submit` tool so
//! the operator tests a channel on exactly the path its plugin uses.
//! `digest` likewise goes through the governed `orbit.decision.digest`
//! tool, raising the approval requests the plugin's digest presents.
//! `channel …` administers this host's registrations in core directly;
//! core refuses registering, revoking and rotating from an agent envelope,
//! and the CLI's pre-dispatch gate already refuses every plain command to a
//! plugin callback, so no running code can mint itself a human.

mod answer;
mod channel_list;
mod channel_register;
mod channel_revoke;
mod channel_rotate_key;
mod command;
mod consolidate;
mod digest;
mod list;
mod raise;
mod reverse;
mod rule_auto;
mod show;
mod submit;
mod support;

pub use command::DecisionCommand;
