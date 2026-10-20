//! The failed-command and candidate-rerun logs of a required validation command.
//!
//! The owner's delivery path ([`super::candidate_validate`]) and a claimed leaf
//! ([`super::claim`]) write the same evidence for a command that did not pass,
//! and readers parse both. Each caller supplies its own identity fields and
//! any fields only it records; the fields and the `failure_kind` cascade they
//! share are built here, so a new failure kind or log field is added once.

use orbit_types::workflow::FlakyValidation;
use serde_json::{Map, Value, json};

use super::baseline::BaselineCheck;
use super::flaky::CandidateRerun;
use super::required::command::RequiredCommandRun;

/// What a failed command's log records about its judgement.
pub(super) struct FailureLog<'a> {
    pub(super) run: &'a RequiredCommandRun,
    pub(super) run_id: &'a str,
    pub(super) tested_head: &'a str,
    pub(super) base_sha: Option<&'a str>,
    /// The rerun of the command on the synchronized base, when one ran.
    pub(super) baseline: Option<&'a BaselineCheck>,
    /// The artifact path the base's own log is attached under.
    pub(super) baseline_path: Option<&'a str>,
    /// The failing tests the bounded replay recovered.
    pub(super) flaky: Option<&'a FlakyValidation>,
    /// Every failing test also fails on the base.
    pub(super) known_red: bool,
    /// A failure that never reached the network is typed `transient` rather
    /// than falling through to the run's own kind. Only a claimed leaf
    /// applies the branch.
    pub(super) network_transient: bool,
}

impl FailureLog<'_> {
    /// The log: the shared fields, then `fields`, the caller's identity and
    /// own-only fields.
    pub(super) fn record(&self, fields: Value) -> Value {
        let run = self.run;
        let mut log = Map::new();
        log.extend([
            ("schema_version".to_string(), json!(1)),
            ("run_id".to_string(), json!(self.run_id)),
            ("tested_head".to_string(), json!(self.tested_head)),
            ("base_sha".to_string(), json!(self.base_sha)),
            ("command".to_string(), json!(run.command)),
            ("exit_code".to_string(), json!(run.exit_code)),
            ("timed_out".to_string(), json!(run.timed_out)),
            ("output".to_string(), json!(run.output)),
            ("failures".to_string(), json!(run.failures)),
            (
                "baseline_reproduces".to_string(),
                json!(self.baseline_reproduces()),
            ),
            ("validation_env".to_string(), run.environment_record()),
            ("failure_kind".to_string(), self.failure_kind()),
            ("flaky".to_string(), json!(self.flaky)),
            (
                "host_output_refusal".to_string(),
                json!(run.host_output_refusal.as_ref().map(|refusal| json!({
                    "reason": refusal.reason.as_str(),
                    "detail": refusal.detail,
                }))),
            ),
            ("network_retries".to_string(), json!(run.network_retries)),
            ("timing".to_string(), json!(run.timing)),
            ("summary".to_string(), json!(run.summary)),
            (
                "baseline".to_string(),
                json!(self.baseline.map(|check| check.record(self.baseline_path))),
            ),
        ]);
        if let Value::Object(fields) = fields {
            log.extend(fields);
        }
        Value::Object(log)
    }

    fn baseline_reproduces(&self) -> bool {
        self.baseline
            .is_some_and(|check| check.reproduces(self.run))
    }

    /// Who the failure belongs to, most specific judgement first: a replay
    /// that passed, a failure the base shares test for test, one it shares
    /// at all, a base that could not run, an unreachable network, then the
    /// run's own kind.
    fn failure_kind(&self) -> Value {
        if self.flaky.is_some() {
            json!("flaky")
        } else if self.known_red {
            json!("known_red")
        } else if self.baseline_reproduces() {
            json!("baseline_red")
        } else if self
            .baseline
            .is_some_and(|check| check.infrastructure_failure().is_some())
        {
            json!("environment")
        } else if self.network_transient {
            json!("transient")
        } else {
            self.run.failure_kind()
        }
    }
}

/// The log of the bounded failing-test replay of a candidate.
pub(super) fn rerun_log(rerun: &CandidateRerun, run_id: &str, tested_head: &str) -> Value {
    json!({
        "schema_version": 1, "role": "candidate_rerun", "run_id": run_id,
        "tested_head": tested_head, "command": rerun.run.command,
        "exit_code": rerun.run.exit_code, "timed_out": rerun.run.timed_out,
        "output": rerun.run.output, "summary": rerun.run.summary,
        "timing": rerun.run.timing,
        "validation_env": rerun.run.environment_record(), "flaky": rerun.flaky,
    })
}
