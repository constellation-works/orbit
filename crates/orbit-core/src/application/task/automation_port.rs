//! What task-owned deterministic steps need from the state scheduler
//! [ORB-17924].
//!
//! Task-pilot prepare and apply, finding verification and escalation triage
//! recheck the scheduler's claim, judge member freshness, fingerprint task
//! material and read which live runs hold a task. `automation` owns all of
//! that, and it also drives these steps, so a direct call from here would
//! close a cycle. This port is the way in: `automation` implements it on
//! [`OrbitRuntime`](crate::OrbitRuntime), so composing the runtime binds it.
//! Two task-pilot callers the task did not list still name `automation`
//! directly, by the owner's stop-and-report ruling: the bounded `Source`
//! reader in `pilot/no_diff.rs` and `pilot_reservation::reserve` in
//! `pilot/prepare.rs`.

use std::collections::{BTreeMap, BTreeSet};

use orbit_automation::AutomationError;
use orbit_common::OrbitError;
use orbit_types::task::Task;
use orbit_types::workflow::automation::members::{MemberAttempt, PreparationPolicy};
use serde_json::Value;

use crate::application::contracts::PilotFingerprints;

/// The state-scheduler operations task steps call, implemented by
/// `automation`.
pub(in crate::application) trait TaskAutomationPort {
    /// The live state claim `value` carries under `state_automation`,
    /// rechecked at a deterministic prepare/apply boundary; `None` when it
    /// carries none.
    fn state_claim(&self, value: &Value) -> Result<Option<MemberAttempt>, OrbitError>;

    /// Which of `task_ids` the branch made stale since `claim` froze its
    /// source, each with the reason.
    fn stale_claimed_tasks(
        &self,
        claim: &MemberAttempt,
        policy: &PreparationPolicy,
        prepared: &Value,
        task_ids: &[String],
        material: &BTreeMap<String, Vec<String>>,
    ) -> Result<BTreeMap<String, String>, OrbitError>;

    /// The freshness policy a run evaluates: its claim's consumer policy, or
    /// the unrouted one without a claim.
    fn claim_preparation_policy(
        &self,
        claim: Option<&MemberAttempt>,
    ) -> Result<PreparationPolicy, OrbitError>;

    /// The material fingerprint of `task` at `revision`.
    fn task_material_fingerprint(
        &self,
        task: &Task,
        revision: &str,
        policy: &PreparationPolicy,
    ) -> Result<String, AutomationError>;

    /// The task-pilot fingerprint, which also covers a workspace with no
    /// pinned source revision.
    fn task_pilot_fingerprint(
        &self,
        task: &Task,
        revision: Option<&str>,
        policy: &PreparationPolicy,
    ) -> Result<String, AutomationError>;

    /// Both task-pilot hashes and the per-field digests, from one read.
    fn task_pilot_fingerprints(
        &self,
        task: &Task,
        revision: &str,
        policy: &PreparationPolicy,
    ) -> Result<PilotFingerprints, AutomationError>;

    /// The per-field freshness digests of `task` at `revision`.
    fn task_freshness_digests(
        &self,
        task: &Task,
        revision: &str,
        policy: &PreparationPolicy,
    ) -> Result<BTreeMap<String, String>, AutomationError>;

    /// Tasks live pilot runs hold, by reservation or prepare checkpoint,
    /// with the holding run IDs.
    fn pilot_preparation_holders(&self) -> Result<BTreeMap<String, BTreeSet<String>>, OrbitError>;

    /// Tasks live finding-verification runs hold, with the holding run IDs.
    fn finding_verification_holders(
        &self,
    ) -> Result<BTreeMap<String, BTreeSet<String>>, OrbitError>;

    /// Tasks live escalation-triage runs hold, with the holding run IDs.
    fn escalation_triage_holders(&self) -> Result<BTreeMap<String, BTreeSet<String>>, OrbitError>;

    /// The prepared checkpoint of each live escalation-triage run holding
    /// `task_id`, by run ID.
    fn escalation_triage_checkpoints(
        &self,
        task_id: &str,
    ) -> Result<BTreeMap<String, Value>, OrbitError>;
}
