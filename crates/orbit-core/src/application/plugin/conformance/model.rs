use super::gate::PluginTestValidationGate;

/// One golden's outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginTestOutcome {
    pub name: String,
    /// Canonical tool name the case called.
    pub tool: String,
    pub passed: bool,
    /// Whether the case was deferred to a host replay without running
    /// ([`PluginTestReport::deferral`]). A deferred case has not passed.
    pub deferred: bool,
    /// Whether `--update-goldens` replaced this case's expected output.
    pub updated: bool,
    /// Why it failed, or empty when it passed.
    pub detail: String,
}

/// What `orbit plugin test <dir>` found.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginTestReport {
    pub name: String,
    pub version: String,
    pub root: String,
    pub manifest_digest: String,
    /// The Orbit version the suite ran against.
    pub orbit_version: String,
    pub results: Vec<PluginTestOutcome>,
    /// Whether the certification was written to this host's plugin record,
    /// and why it was not when it was not.
    pub certified: bool,
    pub certification_note: String,
    /// The manifest's requested grant set, in the same spelling the refusal
    /// prints. `none` when the manifest requests nothing.
    pub requested_grants: String,
    /// Set when no case ran because this lane's sandbox refuses the
    /// backend's, and the run deferred every case to a host replay.
    pub deferral: Option<PluginTestDeferral>,
}

/// A run deferred to the host because `sandbox-exec` cannot apply the
/// backend's profile where it ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginTestDeferral {
    /// The Seatbelt apply probe's refusal.
    pub reason: String,
    /// One `SKIP: sandbox-exec cannot apply a profile:` line per case.
    pub notices: Vec<String>,
}

impl PluginTestReport {
    pub fn passed(&self) -> bool {
        !self.results.is_empty() && self.results.iter().all(|result| result.passed)
    }

    /// The gate's exit status: 0 when every case passed or the whole run
    /// was deferred to a host replay, 1 otherwise.
    pub fn exit_code(&self) -> i32 {
        i32::from(!self.passed() && self.deferral.is_none())
    }

    /// Names of the failing cases, for a one-line summary.
    pub fn failures(&self) -> Vec<&str> {
        self.results
            .iter()
            .filter(|result| !result.passed)
            .map(|result| result.name.as_str())
            .collect()
    }
}

/// How `orbit plugin test` was asked to treat a manifest's requested profile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginTestOptions {
    /// Treat a directory that is not installed at this manifest digest as a
    /// verified first-party checkout, like `orbit plugin validate --first-party`.
    pub first_party: bool,
    /// `--grant` names, parsed with the same rules as `orbit plugin enable`.
    /// Empty means the flag was omitted.
    pub grants: Vec<String>,
    /// `--accept-requested`: run under the manifest's full requested profile.
    pub accept_requested: bool,
    /// `--case <name>`: run exactly one named golden.
    pub case: Option<String>,
    /// Replace mismatched output expectations with the actual output.
    pub update_goldens: bool,
    /// Set when a validation gate runs the suite and wants its summary.
    /// Only such a run may defer to a host replay.
    pub validation_gate: Option<PluginTestValidationGate>,
    /// `ORBIT_REQUIRE_SANDBOX_EXEC=1`: refuse rather than defer when the
    /// backend's sandbox cannot apply here.
    pub require_sandbox_exec: bool,
}
