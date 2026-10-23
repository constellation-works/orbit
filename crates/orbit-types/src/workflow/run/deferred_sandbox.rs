//! The implementer's deferred-sandbox handoff and the record Orbit derives
//! from it [ORB-15287].
//!
//! An agent lane's own sandbox refuses the nested sandbox Orbit's sandbox
//! tests apply, so those tests print a notice and pass without running their
//! confined path: on Linux a [`BUBBLEWRAP_DEFERRAL_PREFIX`] notice, because
//! Bubblewrap cannot create a nested user namespace; on macOS a
//! [`SEATBELT_DEFERRAL_PREFIX`] notice, because `sandbox-exec` cannot apply a
//! nested profile [ORB-15409]. A macOS lane's sandbox also refuses to exec
//! the setuid `ps` that process-identity tests compare against, so they print
//! a [`PS_DENIED_DEFERRAL_PREFIX`] notice; only a claimed leaf on macOS may
//! defer that one ([`DeferralLane`]) [ORB-15587]. An affected-test gate can
//! therefore exit 0 although those paths never ran.
//!
//! The implementer hands that gap off by naming only the validation summary
//! the gate wrote ([`deferral_handoff`]). Orbit builds the
//! [`DeferredSandboxValidation`] itself [ORB-15516]: the gate's command, exit
//! status, executed tests, base and notices come from the summary
//! ([`DeferralHandoff::derive`]), and the probes are ones Orbit runs in the
//! implementer's lane ([`PendingDeferral::complete`]). A gate's notices may
//! be of several kinds, as a macOS claimed leaf's broad gate prints Seatbelt
//! and ps-denied ones together; the record then defers each kind, and Orbit
//! runs one probe per kind, every one of which must refuse [ORB-15692].
//! Orbit enforces the record at two points:
//!
//! - the implement step boundary derives it and fails the step, before
//!   anything is committed, unless it is the sanctioned case: only deferral
//!   notices, each of a kind the lane may defer and whose probe the lane
//!   refused. The derived record replaces the handoff
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

use super::validation_gates::HostValidationGates;
use crate::workflow::host_evidence::{
    BUBBLEWRAP_DEFERRAL_PREFIX, BUBBLEWRAP_NAMESPACE_REFUSALS, EvidenceHostOs,
    PS_DENIED_DEFERRAL_PREFIX, PS_EXEC_REFUSAL, SEATBELT_APPLY_REFUSAL,
    SEATBELT_APPLY_REFUSAL_EXIT, SEATBELT_DEFERRAL_PREFIX, is_bubblewrap_namespace_refusal,
};

/// The task artifact an owner run keeps an accepted record in.
pub const DEFERRED_SANDBOX_ARTIFACT: &str = "deferred-sandbox-validation.json";

/// The probe that shows the lane refuses a nested user namespace.
pub const BUBBLEWRAP_NAMESPACE_PROBE: &str = "bwrap --unshare-user --ro-bind / / -- /bin/true";

/// The probe that shows the lane refuses to apply a nested Seatbelt profile:
/// the permissive profile Orbit's own test guard applies.
pub const SEATBELT_APPLY_PROBE: &str =
    "/usr/bin/sandbox-exec -p '(version 1) (allow default)' /usr/bin/true";

/// The probe that shows the lane refuses to exec `ps` [ORB-15587].
pub const PS_EXEC_PROBE: &str = "/bin/ps -o pid= -p 1";

/// An affected-test gate that passed with only deferrals of sandboxes the
/// implementer's lane refuses, as Orbit derived it from the gate's summary
/// and its own probes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeferredSandboxValidation {
    /// The gate, spelled as the owner's configuration names it. A claimed
    /// leaf reads that spelling from its claim's captured gates, not the
    /// replica's configuration [ORB-15622].
    pub command: String,
    pub exit_code: i32,
    /// Tests the gate executed.
    pub tests_run: u64,
    /// The delivery base the gate compared against: the step's `base_sha`.
    pub base: String,
    /// Every skip or deferral line the gate printed.
    pub notices: Vec<String>,
    /// The deferring sandbox's probe ([`BUBBLEWRAP_NAMESPACE_PROBE`],
    /// [`SEATBELT_APPLY_PROBE`] or [`PS_EXEC_PROBE`]) as Orbit ran it in the
    /// implementer's lane. When the notices defer several sandboxes, the
    /// first one's in Bubblewrap, Seatbelt, ps-denied order.
    pub probe: NamespaceProbe,
    /// The probes of the other sandboxes the notices defer, in that order
    /// [ORB-15692]. Empty, and left out of the encoded record, when they
    /// defer one sandbox, so a single-sandbox record keeps its shape.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub other_probes: Vec<NamespaceProbe>,
}

/// One run of a sandbox probe: [`BUBBLEWRAP_NAMESPACE_PROBE`],
/// [`SEATBELT_APPLY_PROBE`] or [`PS_EXEC_PROBE`].
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

/// Where the implementer that handed a deferral off ran, which decides the
/// kinds it may defer [ORB-15587].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeferralLane {
    /// The implementer ran as a claimed leaf, whose claim host replays the
    /// gate outside its sandbox before handing the candidate to its owner.
    pub claimed: bool,
    /// The OS of the host the implementer ran on.
    pub os: Option<EvidenceHostOs>,
}

/// A sandbox whose refusal a record defers. Every notice is one of the
/// record's sandboxes', and each of them has its own probe. Declaration order
/// is the order of the record's probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DeferringSandbox {
    /// Linux: Bubblewrap cannot create a nested user namespace.
    Bubblewrap,
    /// macOS: `sandbox-exec` cannot apply a nested profile [ORB-15409].
    Seatbelt,
    /// macOS: the lane's sandbox refuses to exec the setuid `ps`. Only a
    /// claimed leaf may defer it [ORB-15587].
    PsDenied,
}

impl DeferringSandbox {
    fn of_notice(notice: &str) -> Option<Self> {
        [Self::Bubblewrap, Self::Seatbelt, Self::PsDenied]
            .into_iter()
            .find(|sandbox| notice.starts_with(sandbox.prefix()))
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::Bubblewrap => BUBBLEWRAP_DEFERRAL_PREFIX,
            Self::Seatbelt => SEATBELT_DEFERRAL_PREFIX,
            Self::PsDenied => PS_DENIED_DEFERRAL_PREFIX,
        }
    }

    /// Why an implementer in `lane` may not defer this kind, if it may not.
    fn refuses_lane(self, lane: DeferralLane) -> Option<String> {
        match self {
            Self::Bubblewrap | Self::Seatbelt => None,
            Self::PsDenied if lane.claimed && lane.os == Some(EvidenceHostOs::Macos) => None,
            Self::PsDenied => Some(format!(
                "lists `{PS_DENIED_DEFERRAL_PREFIX}` notices, which only a claimed leaf on macOS \
                 may defer; this implementer ran {} on {}",
                if lane.claimed {
                    "as a claimed leaf"
                } else {
                    "in its owner's run"
                },
                lane.os.map_or("an unnamed OS", EvidenceHostOs::as_str)
            )),
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
            Self::PsDenied => DeferralProbe {
                command: PS_EXEC_PROBE,
                argv: &["/bin/ps", "-o", "pid=", "-p", "1"],
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
                probe.exit_code > 0 && is_bubblewrap_namespace_refusal(&probe.output)
            }
            Self::Seatbelt => {
                probe.exit_code == SEATBELT_APPLY_REFUSAL_EXIT
                    && probe.output.contains(SEATBELT_APPLY_REFUSAL)
            }
            Self::PsDenied => {
                // The lane's sandbox wrapper ran and its exec of `ps` was
                // refused. As for Bubblewrap, a probe that could not run
                // (-1) justifies nothing, and a wrapper that could not apply
                // its sandbox at all is not one that refuses `ps`.
                probe.exit_code > 0
                    && probe.output.contains(PS_EXEC_REFUSAL)
                    && !probe.output.contains(SEATBELT_APPLY_REFUSAL)
            }
        }
    }

    fn probe_requirement(self) -> String {
        match self {
            Self::Bubblewrap => format!(
                "has no failing namespace probe: in the implementer's lane it must exit \
                 nonzero with `{}`",
                BUBBLEWRAP_NAMESPACE_REFUSALS.join("` or `")
            ),
            Self::Seatbelt => format!(
                "has no refused Seatbelt apply probe: in the implementer's lane it must exit \
                 {SEATBELT_APPLY_REFUSAL_EXIT} with `{SEATBELT_APPLY_REFUSAL}`"
            ),
            Self::PsDenied => format!(
                "has no refused `ps` probe: in the implementer's lane it must exit nonzero \
                 with `{PS_EXEC_REFUSAL}`"
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
    /// Check the gate's `summary` against the `gates` this host holds the
    /// candidate to, the step's `pinned_base` and the `lane` the implementer
    /// ran in. `Ok` names the probe Orbit must run; `Err` is the first reason
    /// the gate run is not deferrable, before any probe runs.
    pub fn derive(
        &self,
        summary: &[u8],
        gates: &HostValidationGates,
        pinned_base: Option<&str>,
        lane: DeferralLane,
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
        if !names_gate(&command, &gates.gates.deferrable()) {
            return Err(format!(
                "names `{command}`, which is not one of the owner's validation gates \
                 (`workflow.required_validation_commands` or `review.baseline_commands`), so no \
                 host would replay it; the gates checked are {}",
                gates.describe_deferrable()
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
        if notices.is_empty() {
            return Err("lists no deferral notice".to_string());
        }
        let mut sandboxes = Vec::new();
        for notice in &notices {
            let Some(sandbox) = DeferringSandbox::of_notice(notice) else {
                return Err(format!(
                    "lists `{notice}`, which is not a Bubblewrap, Seatbelt or ps-denied \
                     deferral; only `{BUBBLEWRAP_DEFERRAL_PREFIX}`, \
                     `{SEATBELT_DEFERRAL_PREFIX}` or `{PS_DENIED_DEFERRAL_PREFIX}` notices are \
                     deferrable"
                ));
            };
            sandboxes.push(sandbox);
        }
        sandboxes.sort_unstable();
        sandboxes.dedup();
        if let Some(refusal) = sandboxes
            .iter()
            .find_map(|sandbox| sandbox.refuses_lane(lane))
        {
            return Err(refusal);
        }
        Ok(PendingDeferral {
            command,
            exit_code,
            tests_run,
            base,
            notices,
            sandboxes,
            handed: self.handed.clone(),
        })
    }
}

/// A deferrable gate run, waiting for the probes that show each of its
/// sandboxes' refusal in the implementer's lane.
#[derive(Debug, Clone)]
pub struct PendingDeferral {
    command: String,
    exit_code: i32,
    tests_run: u64,
    base: String,
    notices: Vec<String>,
    /// The sandboxes the notices defer: at least one, sorted and distinct.
    sandboxes: Vec<DeferringSandbox>,
    handed: Map<String, Value>,
}

impl PendingDeferral {
    /// The probes of the sandboxes the notices defer, one per sandbox, in
    /// the order [`PendingDeferral::complete`] takes their answers.
    #[must_use]
    pub fn probes(&self) -> Vec<DeferralProbe> {
        self.sandboxes
            .iter()
            .map(|sandbox| sandbox.probe())
            .collect()
    }

    /// The record, once `probes`, answered in [`PendingDeferral::probes`]
    /// order, show every sandbox's refusal. `Err` when one is missing, is
    /// another probe or did not refuse, or when a hand-written field differs
    /// from the derivation.
    pub fn complete(
        self,
        probes: Vec<NamespaceProbe>,
    ) -> Result<DeferredSandboxValidation, String> {
        let mut answers = probes.into_iter();
        let mut refused = Vec::with_capacity(self.sandboxes.len());
        for sandbox in &self.sandboxes {
            let expected = sandbox.probe().command;
            let Some(probe) = answers.next() else {
                return Err(format!(
                    "has no `{expected}` probe; every sandbox the notices defer needs its own"
                ));
            };
            if probe.command != expected {
                return Err(format!(
                    "names the probe `{}`, not `{expected}`",
                    probe.command
                ));
            }
            if !sandbox.refused(&probe) {
                return Err(sandbox.probe_requirement());
            }
            refused.push(probe);
        }
        if let Some(extra) = answers.next() {
            return Err(format!(
                "names the probe `{}`, which no notice defers",
                extra.command
            ));
        }
        let mut refused = refused.into_iter();
        let Some(probe) = refused.next() else {
            return Err("lists no deferral notice".to_string());
        };
        let record = DeferredSandboxValidation {
            command: self.command,
            exit_code: self.exit_code,
            tests_run: self.tests_run,
            base: self.base,
            notices: self.notices,
            probe,
            other_probes: refused.collect(),
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
