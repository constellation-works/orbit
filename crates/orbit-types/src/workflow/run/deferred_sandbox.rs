//! The implementer's deferred-sandbox handoff and the record Orbit derives
//! from it [ORB-15287].
//!
//! An agent lane's own sandbox refuses the nested sandbox Orbit's sandbox
//! tests apply, so those tests print a notice and pass without running their
//! confined path: on Linux a [`BUBBLEWRAP_DEFERRAL_PREFIX`] notice, because
//! Bubblewrap cannot create a nested user namespace; on macOS a
//! [`SEATBELT_DEFERRAL_PREFIX`] notice, because `sandbox-exec` cannot apply a
//! nested profile [ORB-15409]. An affected-test gate can therefore exit 0
//! although those paths never ran.
//!
//! The implementer hands that gap off by naming only the validation summary
//! the gate wrote ([`deferral_handoff`]). Orbit builds the
//! [`DeferredSandboxValidation`] itself [ORB-15516]: the gate's command, exit
//! status, executed tests, base and notices come from the summary
//! ([`DeferralHandoff::derive`]), and the probe is one Orbit runs in the
//! implementer's lane ([`PendingDeferral::complete`]). Orbit enforces the
//! record at two points:
//!
//! - the implement step boundary derives it and fails the step, before
//!   anything is committed, unless it is exactly one sandbox's case: its
//!   notices and its refused probe. The derived record replaces the handoff
//!   in the step's output, and an owner run keeps it on the task as
//!   [`DEFERRED_SANDBOX_ARTIFACT`], so a resumed candidate still carries it;
//! - the validation step that runs the owner's required commands outside any
//!   agent sandbox (a claimed leaf's on its claim host) replays the record's
//!   command there before delivery. A replay that still defers is the
//!   validation environment's failure, and one that executed no tests is
//!   refused.
//!
//! The record only ever adds a native run; it never stands in for one.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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
/// Orbit derived it from the gate's summary and its own probe.
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
    /// [`SEATBELT_APPLY_PROBE`]) as Orbit ran it in the implementer's lane.
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
/// implementation deferred, or `None` once a later one deferred nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeferredSandboxArtifact {
    pub schema_version: u32,
    /// The run whose implementer handed it off.
    pub run_id: String,
    pub deferred: Option<DeferredSandboxValidation>,
}

impl DeferredSandboxValidation {
    /// Whether the record's command is one of `gates`, compared trimmed.
    #[must_use]
    pub fn names_gate(&self, gates: &[String]) -> bool {
        names_gate(&self.command, gates)
    }
}

fn names_gate(command: &str, gates: &[String]) -> bool {
    let command = command.trim();
    !command.is_empty() && gates.iter().any(|gate| gate.trim() == command)
}

/// A probe Orbit runs for a deferral: its canonical spelling, recorded as
/// [`NamespaceProbe::command`], and the argv that spawns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeferralProbe {
    pub command: &'static str,
    /// The program, then its arguments.
    pub argv: &'static [&'static str],
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

    fn probe(self) -> DeferralProbe {
        match self {
            Self::Bubblewrap => DeferralProbe {
                command: BUBBLEWRAP_NAMESPACE_PROBE,
                argv: &[
                    "bwrap",
                    "--unshare-user",
                    "--ro-bind",
                    "/",
                    "/",
                    "--",
                    "/bin/true",
                ],
            },
            Self::Seatbelt => DeferralProbe {
                command: SEATBELT_APPLY_PROBE,
                argv: &[
                    "/usr/bin/sandbox-exec",
                    "-p",
                    "(version 1) (allow default)",
                    "/usr/bin/true",
                ],
            },
        }
    }

    /// Whether `probe` shows this sandbox's refusal in the lane.
    fn refused(self, probe: &NamespaceProbe) -> bool {
        match self {
            Self::Bubblewrap => {
                // A missing exit status or a probe that could not run is
                // recorded as -1. Its error may contain the namespace
                // refusal too, but only an executed probe can justify a
                // deferral.
                probe.exit_code > 0 && probe.output.contains(BUBBLEWRAP_NAMESPACE_REFUSAL)
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
                "has no failing namespace probe: in the implementer's lane it must exit \
                 nonzero with `{BUBBLEWRAP_NAMESPACE_REFUSAL}`"
            ),
            Self::Seatbelt => format!(
                "has no refused Seatbelt apply probe: in the implementer's lane it must exit \
                 {SEATBELT_APPLY_REFUSAL_EXIT} with `{SEATBELT_APPLY_REFUSAL}`"
            ),
        }
    }
}

/// The implementer's deferral handoff: the gate's validation summary, by
/// checkout-relative path. Any other key is a hand-written record field,
/// accepted only when it equals Orbit's derivation.
#[derive(Debug, Clone, PartialEq)]
pub struct DeferralHandoff {
    /// The file the gate wrote through `ORBIT_VALIDATION_SUMMARY`.
    pub summary: String,
    handed: Map<String, Value>,
}

/// The implementer's `deferred_sandbox_validation` handoff. `Ok(None)` when
/// it is absent or `null`; `Err` says why it names no summary.
pub fn deferral_handoff(output: &Value) -> Result<Option<DeferralHandoff>, String> {
    let Some(value) = output
        .get("deferred_sandbox_validation")
        .filter(|value| !value.is_null())
    else {
        return Ok(None);
    };
    let mut handed = value.as_object().cloned().unwrap_or_default();
    match handed.remove("summary") {
        Some(Value::String(summary)) if !summary.trim().is_empty() => Ok(Some(DeferralHandoff {
            summary: summary.trim().to_string(),
            handed,
        })),
        _ => Err(
            "names no gate summary: hand off `{\"summary\": \"<path>\"}`, the checkout-relative \
             file the gate wrote through `ORBIT_VALIDATION_SUMMARY`, and Orbit derives the \
             record from it"
                .to_string(),
        ),
    }
}

/// What a gate reports about its own run in its validation summary. The
/// selection a base rerun reads is not needed here.
#[derive(Deserialize)]
struct GateSummary {
    schema_version: u32,
    command: Option<String>,
    exit_code: Option<i32>,
    tests_run: Option<u64>,
    base: Option<String>,
    #[serde(default)]
    notices: Vec<String>,
}

impl DeferralHandoff {
    /// Check the gate's `summary` against the owner's `gates` and the step's
    /// `pinned_base`. `Ok` names the probe Orbit must run; `Err` is the first
    /// reason the gate run is not deferrable, before any probe runs.
    pub fn derive(
        &self,
        summary: &[u8],
        gates: &[String],
        pinned_base: Option<&str>,
    ) -> Result<PendingDeferral, String> {
        let mut decoder = serde_json::Deserializer::from_slice(summary);
        let summary: GateSummary = serde_path_to_error::deserialize(&mut decoder)
            .map_err(|error| format!("is not a gate validation summary: {error}"))?;
        if summary.schema_version != 1 {
            return Err(format!(
                "is a schema {} summary; only schema 1 is read",
                summary.schema_version
            ));
        }
        let Some(command) = summary.command.map(|command| command.trim().to_string()) else {
            return Err(
                "names no gate `command`; only a gate that records its own command can be \
                 deferred"
                    .to_string(),
            );
        };
        if !names_gate(&command, gates) {
            return Err(format!(
                "names `{command}`, which is not one of the owner's validation gates \
                 (`workflow.required_validation_commands` or `review.baseline_commands`), so no \
                 host would replay it"
            ));
        }
        let Some(exit_code) = summary.exit_code else {
            return Err(format!(
                "reports no exit status for `{command}`; the gate did not finish"
            ));
        };
        if exit_code != 0 {
            return Err(format!(
                "reports that `{command}` exited {exit_code}; a failing gate is not deferrable"
            ));
        }
        let tests_run = summary.tests_run.unwrap_or(0);
        if tests_run == 0 {
            return Err(format!("reports that `{command}` executed no tests"));
        }
        let Some(pinned) = pinned_base.map(str::trim).filter(|base| !base.is_empty()) else {
            return Err(
                "cannot be checked: the step names no pinned delivery base (`base_sha`)"
                    .to_string(),
            );
        };
        let base = summary.base.unwrap_or_default().trim().to_string();
        if base != pinned {
            return Err(format!(
                "compared against `{base}`, not the pinned delivery base `{pinned}`"
            ));
        }
        let notices = summary
            .notices
            .iter()
            .map(|notice| notice.trim().to_string())
            .collect::<Vec<_>>();
        let Some(first) = notices.first() else {
            return Err("lists no deferral notice".to_string());
        };
        let Some(sandbox) = DeferringSandbox::of_notice(first) else {
            return Err(format!(
                "lists `{first}`, which is neither a Bubblewrap nor a Seatbelt deferral; only \
                 `{BUBBLEWRAP_DEFERRAL_PREFIX}` or `{SEATBELT_DEFERRAL_PREFIX}` notices are \
                 deferrable"
            ));
        };
        if let Some(notice) = notices
            .iter()
            .find(|notice| DeferringSandbox::of_notice(notice) != Some(sandbox))
        {
            return Err(format!(
                "lists `{notice}`, which is not a {} deferral; a record defers only `{}` \
                 notices, one sandbox's",
                sandbox.name(),
                sandbox.prefix()
            ));
        }
        Ok(PendingDeferral {
            command,
            exit_code,
            tests_run,
            base,
            notices,
            sandbox,
            handed: self.handed.clone(),
        })
    }
}

/// A deferrable gate run, waiting for the probe that shows its sandbox's
/// refusal in the implementer's lane.
#[derive(Debug, Clone)]
pub struct PendingDeferral {
    command: String,
    exit_code: i32,
    tests_run: u64,
    base: String,
    notices: Vec<String>,
    sandbox: DeferringSandbox,
    handed: Map<String, Value>,
}

impl PendingDeferral {
    /// The probe of the sandbox the notices defer.
    #[must_use]
    pub fn probe(&self) -> DeferralProbe {
        self.sandbox.probe()
    }

    /// The record, once `probe` shows the sandbox's refusal. `Err` when the
    /// probe is another one or did not refuse, or when a hand-written field
    /// differs from the derivation.
    pub fn complete(self, probe: NamespaceProbe) -> Result<DeferredSandboxValidation, String> {
        let expected = self.sandbox.probe().command;
        if probe.command != expected {
            return Err(format!(
                "names the probe `{}`, not `{expected}`",
                probe.command
            ));
        }
        if !self.sandbox.refused(&probe) {
            return Err(self.sandbox.probe_requirement());
        }
        let record = DeferredSandboxValidation {
            command: self.command,
            exit_code: self.exit_code,
            tests_run: self.tests_run,
            base: self.base,
            notices: self.notices,
            probe,
        };
        let derived =
            serde_json::to_value(&record).map_err(|error| format!("cannot be encoded: {error}"))?;
        for (field, handed) in &self.handed {
            match derived.get(field) {
                None => {
                    return Err(format!(
                        "hands off unknown field `{field}`; name only `summary`"
                    ));
                }
                Some(value) if value != handed => {
                    return Err(format!(
                        "hands `{field}` as {handed}, but Orbit derives {value} from the gate \
                         summary and its own probe; name only `summary`"
                    ));
                }
                Some(_) => {}
            }
        }
        Ok(record)
    }
}
