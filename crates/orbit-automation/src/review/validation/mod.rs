//! What a reviewer's validation records establish about a candidate
//! [ORB-11528].
//!
//! An honest reviewer records more than the checks that had to pass: the
//! negative control that proves a regression, the deployment it was never
//! authorized to perform, the diagnostic attempt a corrected rerun replaced.
//! Reading every record as a requirement turns that honesty into a refusal,
//! and reading a bare pass verdict as sufficient throws the evidence away.
//! This module sits between the two: the reviewer classifies each record and
//! the rules here decide whether the outcomes support the claim.
//!
//! Both the gate that issues a certificate and the coverage rules that spend
//! one read these rules, so a certificate never means one thing when it is
//! written and another when it is used.
//!
//! A classification is bound to evidence beyond its label and note
//! [ORB-14192]: a negative control names its kind and the in-scope sources it
//! exercises, a failed diagnostic names out-of-scope sources for its
//! failures, and a required check an earlier report revision recorded must
//! still be accounted for. Relabeling a failed required check therefore
//! contradicts its own sources or its retained history instead of clearing it.
//!
//! A counterfactual control names the files it temporarily mutated apart
//! from its sources [ORB-14616]: the mutated file is usually the production
//! code a test-only change guards, outside the candidate's scope. Its sources
//! (the checks that rejected the mutation) stay bound to the scope, and
//! settlement confirms each mutation target came back byte-identical. A
//! target the review's own repair changed comes back to the repaired blob
//! the record declares instead [ORB-16447].
//!
//! A failed check the owner trusts is never a diagnostic [ORB-14684]: a
//! `workflow.required_validation_commands` or `review.baseline_commands`
//! entry passes, or carries a baseline claim settlement reproduces on the
//! pinned base. The reviewer's own sources cannot excuse it, so choosing the
//! `diagnostic` role does not avoid the base rerun.
//!
//! An earlier record that carries an id is accounted for by that id alone
//! [ORB-14370]: the final report carries it forward, whatever its command now
//! reads, or retires it with a reason. Records written without ids keep the
//! command-identity rules.
//!
//! A failed required check the host verified known red [ORB-15546] — on the
//! final candidate it failed only tests the pinned base fails too — keeps
//! its failed outcome. It counts as established only through the
//! [`orbit_types::workflow::HostKnownRed`] entry settlement recorded for it, which the context
//! carries; the reviewer's record alone never asserts it.
//!
//! A run that passed only by deferring a sandbox-confined path executed
//! nothing there [ORB-14334]. Inside an agent lane a test of Orbit's own
//! sandbox prints `DEFERRED: …` and returns, because the lane's sandbox
//! refuses a nested one; the reviewer lists those notices in the record's
//! `deferred`. Such a record reads as `not_run` wherever a pass counts: it is
//! no required pass, no replacement of a superseded attempt and no pass of a
//! host-required command. A result from a host that executed the path, such
//! as owner-fulfilled `host_sandbox_test` evidence, takes its place.

mod command_identity;
mod defects;
mod disclosure;
mod evidence;
mod mutation_paths;

pub use command_identity::same_host_command;
pub use defects::{SkippedCheck, ValidationDefect};
pub use disclosure::{validation_limitations, validation_role_counts};
pub use evidence::{ValidationContext, validation_evidence};
pub use mutation_paths::{MutationTarget, in_scope, mutation_targets, repaired_paths};
