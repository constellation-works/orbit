//! The implementer's `deferred_sandbox_validation` output field [ORB-15287].
//!
//! On Linux an agent lane's own sandbox refuses the nested user namespace
//! Bubblewrap needs, so a test of a Bubblewrap-confined path prints a
//! [`BUBBLEWRAP_DEFERRAL_PREFIX`] notice and passes without running it. An
//! affected-test gate can therefore exit 0 although those paths never ran.
//! The implementer hands that gap off with this record instead of failing,
//! and Orbit enforces it at two points:
//!
//! - the implement step boundary calls [`deferred_sandbox_validation`] and
//!   fails the step, before anything is committed, for a record that is not
//!   exactly that case. An owner run keeps the accepted record on the task as
//!   [`DEFERRED_SANDBOX_ARTIFACT`], so a resumed candidate still carries it;
//! - the validation step that runs the owner's required commands outside any
//!   agent sandbox replays the record's command there before delivery. A
//!   replay that still defers is the validation environment's failure, and
//!   one that executed no tests is refused.
//!
//! The record only ever adds a native run; it never stands in for one.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::workflow::host_evidence::{BUBBLEWRAP_DEFERRAL_PREFIX, BUBBLEWRAP_NAMESPACE_REFUSAL};

/// The task artifact an owner run keeps an accepted record in.
pub const DEFERRED_SANDBOX_ARTIFACT: &str = "deferred-sandbox-validation.json";

/// The probe that shows the lane refuses a nested user namespace.
pub const BUBBLEWRAP_NAMESPACE_PROBE: &str = "bwrap --unshare-user --ro-bind / / -- /bin/true";

/// An affected-test gate that passed with only Bubblewrap deferrals, as the
/// implementer ran it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeferredSandboxValidation {
    /// The gate, spelled as the owner's configuration names it.
    pub command: String,
    pub exit_code: i32,
    /// Tests the gate executed.
    pub tests_run: u64,
    /// The delivery base the gate compared against: the step's `base_sha`.
    pub base: String,
    /// Every skip or deferral line the gate printed.
    pub notices: Vec<String>,
    /// [`BUBBLEWRAP_NAMESPACE_PROBE`] as run in the same lane.
    pub probe: NamespaceProbe,
}

/// One run of [`BUBBLEWRAP_NAMESPACE_PROBE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamespaceProbe {
    pub command: String,
    pub exit_code: i32,
    pub output: String,
}

/// [`DEFERRED_SANDBOX_ARTIFACT`]: the record the task's latest
/// implementation returned, or `None` once a later one deferred nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeferredSandboxArtifact {
    pub schema_version: u32,
    /// The run whose implementer returned it.
    pub run_id: String,
    pub deferred: Option<DeferredSandboxValidation>,
}

impl DeferredSandboxValidation {
    /// Whether the record's command is one of `gates`, compared trimmed.
    #[must_use]
    pub fn names_gate(&self, gates: &[String]) -> bool {
        let command = self.command.trim();
        !command.is_empty() && gates.iter().any(|gate| gate.trim() == command)
    }

    /// The first reason this record is not the sanctioned case.
    fn refusal(&self, gates: &[String], pinned_base: Option<&str>) -> Option<String> {
        let command = self.command.trim();
        if !self.names_gate(gates) {
            return Some(format!(
                "names `{command}`, which is not one of the owner's validation gates \
                 (`workflow.required_validation_commands` or `review.baseline_commands`), so no \
                 host would replay it"
            ));
        }
        if self.exit_code != 0 {
            return Some(format!(
                "reports that `{command}` exited {}; a failing gate is not deferrable",
                self.exit_code
            ));
        }
        if self.tests_run == 0 {
            return Some(format!("reports that `{command}` executed no tests"));
        }
        let Some(pinned) = pinned_base.map(str::trim).filter(|base| !base.is_empty()) else {
            return Some(
                "cannot be checked: the step names no pinned delivery base (`base_sha`)"
                    .to_string(),
            );
        };
        if self.base.trim() != pinned {
            return Some(format!(
                "compared against `{}`, not the pinned delivery base `{pinned}`",
                self.base.trim()
            ));
        }
        if self.notices.is_empty() {
            return Some("lists no deferral notice".to_string());
        }
        if let Some(notice) = self
            .notices
            .iter()
            .find(|notice| !notice.trim_start().starts_with(BUBBLEWRAP_DEFERRAL_PREFIX))
        {
            return Some(format!(
                "lists `{}`, which is not a Bubblewrap deferral; only \
                 `{BUBBLEWRAP_DEFERRAL_PREFIX}` notices are deferrable",
                notice.trim()
            ));
        }
        let probe = self
            .probe
            .command
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if probe != BUBBLEWRAP_NAMESPACE_PROBE {
            return Some(format!(
                "names the probe `{probe}`, not `{BUBBLEWRAP_NAMESPACE_PROBE}`"
            ));
        }
        if self.probe.exit_code == 0 || !self.probe.output.contains(BUBBLEWRAP_NAMESPACE_REFUSAL) {
            return Some(format!(
                "has no failing namespace probe: it must exit nonzero with \
                 `{BUBBLEWRAP_NAMESPACE_REFUSAL}` in the same run"
            ));
        }
        None
    }
}

/// The implementer's `deferred_sandbox_validation`, checked against the
/// owner's `gates` and the step's `pinned_base`. `Ok(None)` when it is absent
/// or `null`; `Err` names the first reason the record is refused.
pub fn deferred_sandbox_validation(
    output: &Value,
    gates: &[String],
    pinned_base: Option<&str>,
) -> Result<Option<DeferredSandboxValidation>, String> {
    let Some(value) = output
        .get("deferred_sandbox_validation")
        .filter(|value| !value.is_null())
    else {
        return Ok(None);
    };
    let record = DeferredSandboxValidation::deserialize(value).map_err(|error| {
        format!(
            "is not a `{{command, exit_code, tests_run, base, notices, probe: {{command, \
             exit_code, output}}}}` record: {error}"
        )
    })?;
    match record.refusal(gates, pinned_base) {
        Some(reason) => Err(reason),
        None => Ok(Some(record)),
    }
}
