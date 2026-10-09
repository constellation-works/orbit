//! Executable steps of a claimed distributed leaf [ORB-12616].
//!
//! A claimed leaf never merges and never completes its task. It implements,
//! commits, publishes where its ship mode requires it, and then stops at a
//! typed handoff the owner alone may act on. The first two activities are that
//! stopping point:
//!
//! - [`claim_validate`] resolves the candidate and its base from Git in the
//!   executor's own worktree, refuses a base the candidate does not descend
//!   from, runs the commands the *owner* requires on that exact candidate, and
//!   attaches one captured log per command to the owner's copy of the task.
//!   On the pull-request route the commands run before the candidate is
//!   pushed, and a second, command-free run of the step pins their results to
//!   the pull request once it exists [ORB-14258]. An empty list runs nothing
//!   and records that no required validation commands are configured, as
//!   [`candidate_validate`](super::candidate_validate::candidate_validate)
//!   does.
//! - [`claim_handoff`] re-observes the same identity, builds the typed
//!   [`TaskHandoff`](orbit_types::workflow::handoff::TaskHandoff), and records it as this claim's durable pending
//!   settlement before anything is sent to the owner.
//! - [`claim_candidate_carry`] is the PR leaf's failure hook: it pushes a
//!   committed candidate the leaf never published to a durable ref on
//!   `origin`, so the task's next claim can resume it on any host
//!   [ORB-14338].
//!
//! Neither activity trusts its input. Task, claim, machine and bound run come
//! from [`RuntimeHost::claim_execution_context`](crate::context::RuntimeHost::claim_execution_context), which the runtime derives
//! from the process worker binding; a payload that disagrees with what Git and
//! that binding say is a refusal, never an override.
//!
//! Commands run exactly as on the owner's own delivery path: the same shell,
//! environment, timeout, output capture and failure text
//! ([`super::required_command`]), the same network reruns and the same
//! comparison of a failure with the base ([`super::baseline`]).

mod carry;
mod delivery;
mod handoff;
mod input;
mod no_diff;
mod observe;
mod published;
mod validation;

pub(super) use carry::carry_to_durable_ref;
pub(in crate::executor::automation) use carry::claim_candidate_carry;
pub(super) use delivery::{delivery, slug};
pub(in crate::executor::automation) use handoff::claim_handoff;
pub(super) use handoff::{carries_implementer_output, implementer_summary, require_clean_checkout};
pub use no_diff::observe_no_diff_candidate;
pub(super) use no_diff::{carries_no_diff_artifacts, import_implementation_evidence};
pub use observe::observe_candidate;
pub(super) use published::observe_accepted_revision;
pub use published::observe_published_candidate;
pub(in crate::executor::automation) use validation::claim_validate;
pub(super) use validation::{NO_REQUIRED_COMMANDS_NOTE, SKIPPED_NO_REQUIRED_COMMANDS};
