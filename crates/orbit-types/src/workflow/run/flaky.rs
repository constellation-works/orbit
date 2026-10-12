//! Evidence for a bounded candidate-only failing-test replay.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Maximum number of failed tests eligible for one isolated candidate replay.
pub const MAX_FLAKY_TESTS: usize = 3;
/// Task history event carrying a [`FlakyValidation`] record.
pub const FLAKY_VALIDATION_EVENT: &str = "flaky_validation";
const MARKER: &str = "[flaky_validation]";

/// Tests that failed a required command, passed on its base, then passed an
/// isolated replay on the same candidate. Outputs are bounded by the runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlakyValidation {
    /// Required command as configured.
    pub command: String,
    /// Comparable passing base commit.
    pub base_sha: String,
    /// Candidate commit on which both attempts ran.
    pub tested_head: String,
    /// Run which observed the flake.
    pub run_id: String,
    /// Exact failing-test identities from the complete original output.
    pub failures: BTreeSet<String>,
    /// Selection handed to the isolated replay, including `failing_tests`.
    pub selection: Value,
    /// Original required-command failure status.
    pub original_exit_code: i32,
    /// Captured original failure output.
    pub original_output: String,
    /// Captured passing base output.
    pub base_output: String,
    /// Captured passing candidate replay output.
    pub rerun_output: String,
    /// Number of tests the passing replay executed.
    pub rerun_tests_run: u64,
    /// Resolved host environment used by the replay.
    pub validation_env: Value,
}

impl FlakyValidation {
    /// Check the record's bounded selection and executed coverage. The owner
    /// also checks its command, run, base and head against the pinned handoff.
    #[must_use]
    pub fn valid(&self) -> bool {
        let Some(tests) = self
            .selection
            .get("failing_tests")
            .and_then(Value::as_array)
        else {
            return false;
        };
        let names = self
            .failures
            .iter()
            .map(|failure| failure.strip_prefix("test "))
            .collect::<Option<BTreeSet<_>>>();
        let selected = tests
            .iter()
            .map(Value::as_str)
            .collect::<Option<BTreeSet<_>>>();
        !self.command.trim().is_empty()
            && !self.base_sha.trim().is_empty()
            && !self.tested_head.trim().is_empty()
            && !self.run_id.trim().is_empty()
            && self.original_exit_code != 0
            && !self.failures.is_empty()
            && self.failures.len() <= MAX_FLAKY_TESTS
            && names
                .as_ref()
                .is_some_and(|names| names.iter().all(|name| !name.trim().is_empty()))
            && names == selected
            && tests.len() == self.failures.len()
            && self.rerun_tests_run >= tests.len() as u64
    }

    /// Structured task history note with the human-readable explanation.
    #[must_use]
    pub fn text(&self) -> String {
        let record = serde_json::to_string(self).unwrap_or_default();
        format!(
            "{MARKER} {record} Required validation failed, the base passed, and all named tests passed one isolated candidate replay; delivery proceeds."
        )
    }

    /// Recover the structured record from a history note.
    #[must_use]
    pub fn from_text(text: &str) -> Option<Self> {
        let (_, rest) = text.split_once(MARKER)?;
        serde_json::Deserializer::from_str(rest.trim_start())
            .into_iter::<Self>()
            .next()?
            .ok()
            .filter(Self::valid)
    }
}
