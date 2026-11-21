//! `orbit resident metrics`: the four widening measures of design §9,
//! derived read-only from task history, decision requests, checkpoints and
//! audit rows. No new store: checkpoints and answers are assignment
//! comments, cycles and actions are assignment history, escalations are
//! decision requests.
//!
//! Definitions (design §9):
//! - manual interventions: human writes on the assignment or watched tasks
//!   that are not answers to a resident escalation and not grants —
//!   requeues, cancels, manual status changes, operator steering comments,
//!   unprompted relays (`resident.answer` without a `request_id`) and
//!   `stop`/`takeover` writes — per mandate and per active mandate-day.
//! - escalation load: resident-raised decision requests per mandate-day and
//!   per human-day, median answer latency, share answered with the
//!   recommended option, expired-unanswered count.
//! - reversal rate: share of resident decisions and actions a human undid
//!   within 7 days (the reversal window), each counted once.
//! - recovery cost: cycles that ended without an agent checkpoint, time
//!   from the dead cycle's start to the next good checkpoint, recovery
//!   cycle slot minutes, repeated actions and human interventions needed.
//!
//! `--baseline <task>` computes the same measures for an interactive
//! orchestration task; a measure it cannot derive is the string
//! `not_derivable`, never zero.
//!
//! A referenced task (watched, related, a checkpoint subject or request
//! task) that is not in this workspace is read through its registered
//! owner on this machine: its history and decision requests count, its
//! comments do not (no cross-workspace comment read exists). A reference
//! no registered workspace holds is listed as unreachable and skipped, so
//! one foreign or dangling id never fails the whole report [ORB-21642].

mod measure;
mod report;
mod window;

pub(super) use window::parse_window_start;
