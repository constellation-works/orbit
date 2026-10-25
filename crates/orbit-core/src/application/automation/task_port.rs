//! The state scheduler's side of the task-owned automation port
//! [ORB-17924], [ORB-17943].

use std::collections::{BTreeMap, BTreeSet};

use orbit_automation::AutomationError;
use orbit_common::OrbitError;
use orbit_types::task::Task;
use orbit_types::workflow::automation::members::{MemberAttempt, PreparationPolicy};
use serde_json::Value;

use super::source::Source;
use super::{members, pilot_reservation, preparation};
use crate::OrbitRuntime;
use crate::application::contracts::PilotFingerprints;
use crate::application::task::{SourceGit, TaskAutomationPort};

impl SourceGit for Source<'_> {
    fn git(&self, args: &[&str]) -> Result<String, AutomationError> {
        Source::git(self, args)
    }

    fn git_preserving_output(&self, args: &[&str]) -> Result<String, AutomationError> {
        Source::git_preserving_output(self, args)
    }
}

impl TaskAutomationPort for OrbitRuntime {
    fn bounded_source(&self) -> Box<dyn SourceGit + '_> {
        Box::new(Source::new(&self.paths().repo_root))
    }

    fn reserve_pilot_tasks(
        &self,
        run_id: Option<&str>,
        task_ids: &[String],
    ) -> Result<BTreeMap<String, BTreeSet<String>>, OrbitError> {
        pilot_reservation::reserve(self, run_id, task_ids)
    }

    fn state_claim(&self, value: &Value) -> Result<Option<MemberAttempt>, OrbitError> {
        members::claim(self, value)
    }

    fn stale_claimed_tasks(
        &self,
        claim: &MemberAttempt,
        policy: &PreparationPolicy,
        prepared: &Value,
        task_ids: &[String],
        material: &BTreeMap<String, Vec<String>>,
    ) -> Result<BTreeMap<String, String>, OrbitError> {
        members::stale_tasks(self, claim, policy, prepared, task_ids, material)
    }

    fn claim_preparation_policy(
        &self,
        claim: Option<&MemberAttempt>,
    ) -> Result<PreparationPolicy, OrbitError> {
        preparation::claim_policy(self, claim)
    }

    fn task_material_fingerprint(
        &self,
        task: &Task,
        revision: &str,
        policy: &PreparationPolicy,
    ) -> Result<String, AutomationError> {
        preparation::fingerprint(self, task, revision, policy)
    }

    fn task_pilot_fingerprint(
        &self,
        task: &Task,
        revision: Option<&str>,
        policy: &PreparationPolicy,
    ) -> Result<String, AutomationError> {
        preparation::pilot_fingerprint(self, task, revision, policy)
    }

    fn task_pilot_fingerprints(
        &self,
        task: &Task,
        revision: &str,
        policy: &PreparationPolicy,
    ) -> Result<PilotFingerprints, AutomationError> {
        preparation::fingerprints(self, task, revision, policy)
    }

    fn task_freshness_digests(
        &self,
        task: &Task,
        revision: &str,
        policy: &PreparationPolicy,
    ) -> Result<BTreeMap<String, String>, AutomationError> {
        preparation::component_digests(self, task, revision, policy)
    }

    fn pilot_preparation_holders(&self) -> Result<BTreeMap<String, BTreeSet<String>>, OrbitError> {
        preparation::active_task_pilot_preparations(self)
    }

    fn finding_verification_holders(
        &self,
    ) -> Result<BTreeMap<String, BTreeSet<String>>, OrbitError> {
        preparation::active_finding_verifications(self)
    }

    fn escalation_triage_holders(&self) -> Result<BTreeMap<String, BTreeSet<String>>, OrbitError> {
        preparation::active_escalation_triages(self)
    }

    fn escalation_triage_checkpoints(
        &self,
        task_id: &str,
    ) -> Result<BTreeMap<String, Value>, OrbitError> {
        preparation::escalation_triage_checkpoints_holding(self, task_id)
    }
}
