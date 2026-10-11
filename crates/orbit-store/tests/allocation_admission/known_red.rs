//! A claimed leaf's known-red validation log at owner acceptance [ORB-15427].

use std::collections::BTreeSet;

use orbit_types::workflow::handoff::HandoffKnownRed;
use orbit_types::workflow::{BaselineRedFailures, KNOWN_RED_VALIDATION_EVENT, KnownRedValidation};

use super::*;

/// The command whose log fails known red; the owner also requires `build`.
const RED: &str = "test";
const FAILURES: [&str; 2] = ["test suite::broken", "test suite::flaky"];

fn failures(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(ToString::to_string).collect()
}

impl Delivery {
    /// Replace the handoff's [`RED`] log with one that failed with
    /// `exit_code`, carrying `report`, persisted on the owner like the leaf's.
    fn red_log(&mut self, exit_code: i32, report: Option<HandoffKnownRed>) {
        let handoff = &self.handoff;
        let log = HandoffValidationLog {
            schema_version: 1,
            workspace_id: handoff.workspace_id.clone(),
            task_id: handoff.task_id.clone(),
            claim_id: handoff.claim_id.clone(),
            machine_id: handoff.machine_id.clone(),
            run_id: handoff.run_id.clone(),
            candidate: handoff.candidate.clone(),
            tested_head: handoff.candidate.candidate.commit.clone(),
            command: RED.into(),
            exit_code,
            output: FAILURES.join("\n"),
            known_red: report,
        };
        let artifact = TaskArtifact {
            path: format!("{RED}.known-red.json"),
            content: serde_json::to_vec(&log).unwrap(),
            media_type: "application/json".into(),
            created_by: None,
        };
        let reference = HandoffArtifactRef {
            path: artifact.path.clone(),
            sha256: lower_hex(Sha256::digest(&artifact.content)),
        };
        self.owner
            .backends
            .commit_boundary
            .mutate_execution_claim(
                Some(&self.worker()),
                "known-red-log",
                &ClaimMutation::Evidence(ClaimEvidence {
                    artifacts: vec![artifact],
                    ..Default::default()
                }),
            )
            .expect("persist the known-red log on the owner");
        let slot = self
            .handoff
            .validation
            .iter_mut()
            .find(|existing| existing.path == format!("{RED}.json"))
            .expect("the handoff pins a log for the red command");
        *slot = reference;
    }

    fn report(&self) -> HandoffKnownRed {
        HandoffKnownRed {
            base_sha: self.handoff.candidate.base.commit.clone(),
            selection: None,
            failures: failures(&FAILURES),
        }
    }

    /// The owner's run of [`RED`] on the handed-off base, judged `verdict`.
    fn owner_ran(&mut self, verdict: Result<BTreeSet<String>, String>) {
        self.known_red = vec![KnownRedObservation {
            command: RED.into(),
            base_sha: self.handoff.candidate.base.commit.clone(),
            verdict,
        }];
    }

    fn known_red_history(&self) -> Vec<KnownRedValidation> {
        self.owner
            .backends
            .task
            .history
            .get_task_history(&self.claim.task_id)
            .unwrap()
            .unwrap()
            .into_iter()
            .filter(|entry| entry.event == KNOWN_RED_VALIDATION_EVENT)
            .map(|entry| KnownRedValidation::from_text(entry.note.as_deref().unwrap()).unwrap())
            .collect()
    }
}

/// [ORB-15427] A claimed leaf's failing validation log is accepted only as
/// known red, and only on the owner's own word: the owner's run of the same
/// command on the handed-off base must fail exactly the failures the log
/// reports. Acceptance then records the known-red tests in the task's
/// history, and approval rechecks the pinned log without rerunning anything.
/// A failing log with no report, a report for another base, a report the
/// owner never checked, one its base run contradicts or judged otherwise,
/// and a passing log that claims known red are each refused, leaving the
/// task in progress with nothing recorded.
#[test]
fn a_known_red_log_is_accepted_only_when_the_owners_base_run_fails_its_tests() {
    if !isolated(
        "known_red::a_known_red_log_is_accepted_only_when_the_owners_base_run_fails_its_tests",
    ) {
        return;
    }
    type Spoil = fn(&mut Delivery);
    let refusals: [(&str, Spoil, &str); 6] = [
        (
            "no report",
            |delivery| delivery.red_log(100, None),
            "validation must pass",
        ),
        (
            "another base",
            |delivery| {
                let mut report = delivery.report();
                report.base_sha = "e".repeat(40);
                delivery.red_log(100, Some(report));
            },
            "known_red_unverified",
        ),
        (
            "unchecked by the owner",
            |delivery| delivery.red_log(100, Some(delivery.report())),
            "has not run it",
        ),
        (
            "contradicted by the owner's base run",
            |delivery| {
                delivery.red_log(100, Some(delivery.report()));
                delivery.owner_ran(Err("base passes it".into()));
            },
            "base passes it",
        ),
        (
            "judged otherwise by the owner",
            |delivery| {
                delivery.red_log(100, Some(delivery.report()));
                delivery.owner_ran(Ok(failures(&FAILURES[..1])));
            },
            "other failures",
        ),
        (
            "a passing log claiming known red",
            |delivery| delivery.red_log(0, Some(delivery.report())),
            "validation must pass",
        ),
    ];
    for (case, spoil, refusal) in refusals {
        let mut delivery = Delivery::admit(owner_request("first").ship);
        spoil(&mut delivery);
        let error = delivery
            .accept(None)
            .expect_err(&format!("{case}: refused"))
            .to_string();
        assert!(error.contains(refusal), "{case}: {error}");
        assert_eq!(
            delivery.owner.task_status(&delivery.claim.task_id),
            TaskStatus::InProgress,
            "{case}"
        );
        assert!(delivery.known_red_history().is_empty(), "{case}");
    }

    let mut delivery = Delivery::admit(owner_request("first").ship);
    delivery.red_log(100, Some(delivery.report()));
    delivery.owner_ran(Ok(failures(&FAILURES)));
    delivery
        .accept(None)
        .expect("the owner's base run fails every reported test");
    assert_eq!(
        delivery.owner.task_status(&delivery.claim.task_id),
        TaskStatus::Review
    );
    assert_eq!(
        delivery.known_red_history(),
        vec![KnownRedValidation {
            command: RED.into(),
            base_sha: delivery.handoff.candidate.base.commit.clone(),
            tested_head: delivery.handoff.candidate.candidate.commit.clone(),
            run_id: delivery.handoff.run_id.clone(),
            failures: BaselineRedFailures::new(FAILURES.map(String::from), ""),
        }]
    );
    delivery.known_red.clear();
    delivery
        .approve()
        .expect("approval rechecks the accepted known-red log");
}
