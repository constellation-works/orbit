//! `orbit plugin test` as a validation gate inside an agent lane [ORB-19224].
//!
//! A macOS worker runs its agent under a Seatbelt profile, and Seatbelt
//! refuses to apply a second profile inside one (`sandbox-exec` exits 71 with
//! `sandbox_apply: Operation not permitted`). A conformance run there cannot
//! confine the plugin backend under its declared sandbox, and running the
//! backend under the worker's profile instead would be weaker containment
//! than the plugin declared. So the run never starts a confined backend in
//! such a lane. It takes one of two answers instead, both decided before any
//! case runs:
//!
//! - **Deferred to the host.** A gate run that asked for a validation summary
//!   (`ORBIT_VALIDATION_SUMMARY`) records every selected case as deferred,
//!   prints the sanctioned `SKIP: sandbox-exec cannot apply a profile:` notice
//!   for each, exits 0 and writes the summary an implementer hands off as a
//!   deferred sandbox validation. The claim host then replays the gate
//!   outside the worker sandbox, where the backend runs under its own profile,
//!   and that replay is the validation evidence.
//! - **Refused, typed.** Any other run (or one with
//!   `ORBIT_REQUIRE_SANDBOX_EXEC=1`) fails with the
//!   `[validation_route_unavailable]` marker, which recovery and claim
//!   settlement read as the lane's failure, not the candidate's.
//!
//! A backend the manifest runs unconfined (`sandbox: none` with consent)
//! does not use Seatbelt and is unaffected; neither is Linux, whose Landlock
//! nests inside the worker's sandbox.

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_exec::SandboxExecApplyProbe;
use orbit_tools::plugin::PluginSandboxProfile;
use orbit_types::workflow::VALIDATION_ROUTE_UNAVAILABLE_MARKER;
use serde_json::json;

use super::conformance::{PluginTestOptions, PluginTestReport};

/// Where a validation gate asked `orbit plugin test` to report its run: the
/// `ORBIT_VALIDATION_SUMMARY`, `ORBIT_VALIDATION_GATE` and
/// `ORBIT_VALIDATION_BASE` a gate runner sets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginTestValidationGate {
    /// The file the run writes its validation summary to.
    pub summary: PathBuf,
    /// The gate as the owner's configuration spells it, recorded as the
    /// summary's `command`.
    pub command: Option<String>,
    /// The delivery base the gate runs for, recorded as the summary's `base`.
    pub base: Option<String>,
}

/// Why this lane cannot confine `profile`'s backend, when the run must defer
/// it to the host instead of starting it. `Ok(None)` runs the cases as usual;
/// `Err` is the typed refusal for a run that cannot defer.
pub(super) fn seatbelt_deferral(
    profile: &PluginSandboxProfile,
    options: &PluginTestOptions,
    namespace: &str,
) -> Result<Option<String>, OrbitError> {
    if profile.unsandboxed {
        return Ok(None);
    }
    let SandboxExecApplyProbe::ApplyRefused(reason) = orbit_exec::probe_sandbox_exec_apply() else {
        // Applied, not macOS, or a wrapper that is absent or failed: the
        // backend spawn keeps its fail-closed handling of each.
        return Ok(None);
    };
    let refuse = |why: &str| {
        Err(OrbitError::PolicyDenied(format!(
            "{VALIDATION_ROUTE_UNAVAILABLE_MARKER} plugin '{namespace}' runs its backend under \
             sandbox-exec, which cannot apply a profile here ({reason}): this process is already \
             inside a Seatbelt sandbox, such as a macOS Orbit worker's. No conformance case ran, \
             and none runs under weaker containment than the plugin declares. {why}"
        )))
    };
    if options.require_sandbox_exec {
        return refuse("ORBIT_REQUIRE_SANDBOX_EXEC=1 refuses to defer it.");
    }
    if options.validation_gate.is_none() {
        return refuse(
            "Run it outside the enclosing sandbox, or, as a required validation gate in an Orbit \
             worker, with ORBIT_VALIDATION_SUMMARY (plus ORBIT_VALIDATION_GATE and \
             ORBIT_VALIDATION_BASE) set, so the run defers to the host replay.",
        );
    }
    Ok(Some(reason.clone()))
}

/// Write the run's validation summary where the gate asked for it.
///
/// `tests_run` counts the cases the run entered, deferred ones included, as
/// a Cargo gate counts a test that returned at its Seatbelt guard; a host
/// replay that still defers is refused by the validation step.
pub(super) fn write_validation_summary(
    gate: &PluginTestValidationGate,
    report: &PluginTestReport,
) -> Result<(), OrbitError> {
    let notices = report
        .deferral
        .as_ref()
        .map(|deferral| deferral.notices.clone())
        .unwrap_or_default();
    let summary = json!({
        "schema_version": 1,
        "command": gate.command,
        "base": gate.base,
        "exit_code": report.exit_code(),
        "selection": {
            "plugin": report.name,
            "cases": report.results.iter().map(|result| &result.name).collect::<Vec<_>>(),
        },
        "tests_run": report.results.len(),
        "notices": notices,
    });
    let mut text = serde_json::to_string_pretty(&summary)
        .map_err(|error| OrbitError::Io(format!("encode the validation summary: {error}")))?;
    text.push('\n');
    atomic_write_text(&gate.summary, &text).map_err(|error| {
        OrbitError::Io(format!(
            "write the validation summary {}: {error}",
            gate.summary.display()
        ))
    })
}
