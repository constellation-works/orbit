//! The validation gates a host holds a candidate to, and where it read them
//! [ORB-15622].
//!
//! An owner's gates are its `workflow.required_validation_commands` and
//! `review.baseline_commands`. Per-host `.orbit/` configuration is untracked,
//! so a replica's copy can drift from the owner's silently. Pull admission
//! therefore captures the owner's gates in each claim's receipt, and a claimed
//! leaf validates, replays deferrals and trusts baseline reruns against that
//! capture rather than its own configuration.

use serde::{Deserialize, Serialize};

/// An owner's gates, as admission captures them and the probe reports them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationGates {
    /// `workflow.required_validation_commands`.
    #[serde(default)]
    pub required_validation_commands: Vec<String>,
    /// `review.baseline_commands`.
    #[serde(default)]
    pub baseline_commands: Vec<String>,
}

impl ValidationGates {
    /// The gates whose sandbox deferrals an implementer may hand off: the
    /// required commands, then each baseline command they do not already
    /// hold, compared trimmed.
    #[must_use]
    pub fn deferrable(&self) -> Vec<String> {
        let mut gates = self.required_validation_commands.clone();
        for baseline in &self.baseline_commands {
            if !gates.iter().any(|gate| gate.trim() == baseline.trim()) {
                gates.push(baseline.trim().to_string());
            }
        }
        gates
    }

    /// How these gates, a replica's own, differ from the `owner`'s: one
    /// line per differing key, compared trimmed and in order. Empty when
    /// they match.
    #[must_use]
    pub fn drift_from(&self, owner: &Self) -> Vec<String> {
        [
            (
                "workflow.required_validation_commands",
                &self.required_validation_commands,
                &owner.required_validation_commands,
            ),
            (
                "review.baseline_commands",
                &self.baseline_commands,
                &owner.baseline_commands,
            ),
        ]
        .into_iter()
        .filter(|(_, replica, owner)| trimmed(replica) != trimmed(owner))
        .map(|(key, replica, owner)| {
            format!(
                "`{key}` is {} here but {} on the owner",
                listed(replica),
                listed(owner)
            )
        })
        .collect()
    }
}

/// Where a host read the [`ValidationGates`] it applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationGateSource {
    /// This host's own configuration: an owner validating its own runs.
    LocalConfig,
    /// The owner's gates, captured in the claim's admission contract.
    OwnerContract,
    /// A claimed leaf whose admission captured no owner gates (an older
    /// owner): this replica's own configuration stands in for them.
    ReplicaConfig,
}

impl ValidationGateSource {
    /// The source, as a refusal names it.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::LocalConfig => "this host's configuration",
            Self::OwnerContract => "the owner's gates captured in this claim's admission contract",
            Self::ReplicaConfig => {
                "this replica's local configuration, because this claim's admission contract \
                 captured no owner gates"
            }
        }
    }
}

/// The gates a host applies, with where it read them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostValidationGates {
    pub gates: ValidationGates,
    pub source: ValidationGateSource,
}

impl HostValidationGates {
    /// Required commands a managed invocation may leave to final validation.
    /// A replica fallback is not the owner's captured contract; an empty
    /// list conveys no ownership.
    #[must_use]
    pub fn captured_required_commands(&self) -> Option<&[String]> {
        (self.source != ValidationGateSource::ReplicaConfig
            && !self.gates.required_validation_commands.is_empty())
        .then_some(self.gates.required_validation_commands.as_slice())
    }

    /// Gates read from this host's own configuration.
    #[must_use]
    pub fn local(gates: ValidationGates) -> Self {
        Self {
            gates,
            source: ValidationGateSource::LocalConfig,
        }
    }

    /// The deferrable gates and their source, as a refusal names them.
    #[must_use]
    pub fn describe_deferrable(&self) -> String {
        format!(
            "{} from {}",
            listed(&self.gates.deferrable()),
            self.source.describe()
        )
    }
}

fn trimmed(commands: &[String]) -> Vec<&str> {
    commands.iter().map(|command| command.trim()).collect()
}

fn listed(commands: &[String]) -> String {
    if commands.is_empty() {
        return "[]".to_string();
    }
    format!(
        "[{}]",
        commands
            .iter()
            .map(|command| format!("`{}`", command.trim()))
            .collect::<Vec<_>>()
            .join(", ")
    )
}
