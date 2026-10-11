//! The implementer's `deferred_sandbox_validation` output field [ORB-15287].
//!
//! An agent lane's own sandbox refuses the nested sandbox Orbit's sandbox
//! tests apply, so those tests print a notice and pass without running their
//! confined path: on Linux a [`BUBBLEWRAP_DEFERRAL_PREFIX`] notice, because
//! Bubblewrap cannot create a nested user namespace; on macOS a
//! [`SEATBELT_DEFERRAL_PREFIX`] notice, because `sandbox-exec` cannot apply a
//! nested profile [ORB-15409]. An affected-test gate can therefore exit 0
//! although those paths never ran. The implementer hands that gap off with
//! this record instead of failing, and Orbit enforces it at two points:
//!
//! - the implement step boundary calls [`deferred_sandbox_validation`] and
//!   fails the step, before anything is committed, for a record that is not
//!   exactly one sandbox's case: its notices and its probe. An owner run keeps
//!   the accepted record on the task as [`DEFERRED_SANDBOX_ARTIFACT`], so a
//!   resumed candidate still carries it;
//! - the validation step that runs the owner's required commands outside any
//!   agent sandbox (a claimed leaf's on its claim host) replays the record's
//!   command there before delivery. A replay that still defers is the
//!   validation environment's failure, and one that executed no tests is
//!   refused.
//!
//! The record only ever adds a native run; it never stands in for one.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::workflow::host_evidence::{
    BUBBLEWRAP_DEFERRAL_PREFIX, BUBBLEWRAP_NAMESPACE_REFUSAL, SEATBELT_APPLY_REFUSAL,
    SEATBELT_APPLY_REFUSAL_EXIT, SEATBELT_DEFERRAL_PREFIX,
};

/// The task artifact an owner run keeps an accepted record in.
pub const DEFERRED_SANDBOX_ARTIFACT: &str = "deferred-sandbox-validation.json";

/// The probe that shows the lane refuses a nested user namespace.
pub const BUBBLEWRAP_NAMESPACE_PROBE: &str = "bwrap --unshare-user --ro-bind / / -- /bin/true";

/// The probe that shows the lane refuses to apply a nested Seatbelt profile:
/// the permissive profile Orbit's own test guard applies.
pub const SEATBELT_APPLY_PROBE: &str =
    "/usr/bin/sandbox-exec -p '(version 1) (allow default)' /usr/bin/true";

/// An affected-test gate that passed with only one sandbox's deferrals, as
/// the implementer ran it.
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
    /// The deferring sandbox's probe ([`BUBBLEWRAP_NAMESPACE_PROBE`] or
    /// [`SEATBELT_APPLY_PROBE`]) as run in the same lane.
    pub probe: NamespaceProbe,
}

/// One run of a sandbox probe: [`BUBBLEWRAP_NAMESPACE_PROBE`] or
/// [`SEATBELT_APPLY_PROBE`].
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
        let first = self.notices[0].trim();
        let Some(sandbox) = DeferringSandbox::of_notice(first) else {
            return Some(format!(
                "lists `{first}`, which is neither a Bubblewrap nor a Seatbelt deferral; only \
                 `{BUBBLEWRAP_DEFERRAL_PREFIX}` or `{SEATBELT_DEFERRAL_PREFIX}` notices are \
                 deferrable"
            ));
        };
        if let Some(notice) = self
            .notices
            .iter()
            .find(|notice| DeferringSandbox::of_notice(notice.trim()) != Some(sandbox))
        {
            return Some(format!(
                "lists `{}`, which is not a {} deferral; a record defers only `{}` notices, \
                 one sandbox's",
                notice.trim(),
                sandbox.name(),
                sandbox.prefix()
            ));
        }
        let probe = self
            .probe
            .command
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if probe != sandbox.probe() {
            return Some(format!(
                "names the probe `{probe}`, not `{}`",
                sandbox.probe()
            ));
        }
        if !sandbox.refused(&self.probe) {
            return Some(sandbox.probe_requirement());
        }
        None
    }
}

/// The sandbox whose refusal a record defers. A record defers one sandbox:
/// every notice and the probe are that sandbox's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeferringSandbox {
    /// Linux: Bubblewrap cannot create a nested user namespace.
    Bubblewrap,
    /// macOS: `sandbox-exec` cannot apply a nested profile [ORB-15409].
    Seatbelt,
}

impl DeferringSandbox {
    fn of_notice(notice: &str) -> Option<Self> {
        [Self::Bubblewrap, Self::Seatbelt]
            .into_iter()
            .find(|sandbox| notice.starts_with(sandbox.prefix()))
    }

    fn name(self) -> &'static str {
        match self {
            Self::Bubblewrap => "Bubblewrap",
            Self::Seatbelt => "Seatbelt",
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::Bubblewrap => BUBBLEWRAP_DEFERRAL_PREFIX,
            Self::Seatbelt => SEATBELT_DEFERRAL_PREFIX,
        }
    }

    fn probe(self) -> &'static str {
        match self {
            Self::Bubblewrap => BUBBLEWRAP_NAMESPACE_PROBE,
            Self::Seatbelt => SEATBELT_APPLY_PROBE,
        }
    }

    /// Whether `probe` shows this sandbox's refusal in the lane.
    fn refused(self, probe: &NamespaceProbe) -> bool {
        match self {
            Self::Bubblewrap => {
                probe.exit_code != 0 && probe.output.contains(BUBBLEWRAP_NAMESPACE_REFUSAL)
            }
            Self::Seatbelt => {
                probe.exit_code == SEATBELT_APPLY_REFUSAL_EXIT
                    && probe.output.contains(SEATBELT_APPLY_REFUSAL)
            }
        }
    }

    fn probe_requirement(self) -> String {
        match self {
            Self::Bubblewrap => format!(
                "has no failing namespace probe: it must exit nonzero with \
                 `{BUBBLEWRAP_NAMESPACE_REFUSAL}` in the same run"
            ),
            Self::Seatbelt => format!(
                "has no refused Seatbelt apply probe: it must exit \
                 {SEATBELT_APPLY_REFUSAL_EXIT} with `{SEATBELT_APPLY_REFUSAL}` in the same run"
            ),
        }
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
    let record: DeferredSandboxValidation =
        serde_path_to_error::deserialize(value).map_err(|error| {
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
