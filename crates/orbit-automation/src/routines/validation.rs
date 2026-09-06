//! Local-only routine-pin validation [ORB-10730, ADR-0358].
//!
//! v1 resolves committed pins from this machine's `host.toml` identity and
//! owner names present in its machine-local workspace registry. It never reads
//! the dormant fleet registry, presence projection, satellite cache, or
//! `last_seen`. See `docs/design/host-registry/2_design.md` §2.1 and §6.

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_types::workspace::WorkspaceRegistry;
use serde::Serialize;

use super::loader::RoutineOrigin;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutineDiagnosticSeverity {
    Warning,
    Error,
}

impl RoutineDiagnosticSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoutineValidationDiagnostic {
    pub code: &'static str,
    pub severity: RoutineDiagnosticSeverity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
    pub message: String,
    /// Retained in the output shape for compatibility. Local v1 conclusions
    /// are never stale because they come from authoritative local files.
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoutineRegistryStatus {
    pub source: &'static str,
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_seconds: Option<u64>,
    pub diagnostics: Vec<RoutineValidationDiagnostic>,
}

impl Default for RoutineRegistryStatus {
    fn default() -> Self {
        Self {
            source: "local_workspace_registry",
            state: "not_evaluated",
            age_seconds: None,
            diagnostics: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoutinePinValidation {
    pub eligible: bool,
    pub diagnostics: Vec<RoutineValidationDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineHostIdentity {
    pub machine_id: String,
    pub host_id: String,
}

pub trait RoutineHostIdentityView {
    fn machine_id(&self) -> &str;
    fn host_id(&self) -> &str;
}

impl RoutineHostIdentityView for RoutineHostIdentity {
    fn machine_id(&self) -> &str {
        &self.machine_id
    }

    fn host_id(&self) -> &str {
        &self.host_id
    }
}

/// Local owner names that are recognizable to routine validation. The local
/// host name is carried separately by [`RoutineHostIdentity`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineRegistryView {
    pub owner_host_ids: BTreeSet<String>,
}

impl RoutineRegistryView {
    pub fn from_workspace_registry(registry: &WorkspaceRegistry, local_machine_id: &str) -> Self {
        let owner_host_ids = registry
            .workspaces
            .iter()
            .filter_map(|workspace| workspace.owner_machine_id.as_deref())
            .filter(|machine_id| *machine_id != local_machine_id)
            .map(|machine_id| {
                registry
                    .owner_host_ids
                    .get(machine_id)
                    .cloned()
                    .unwrap_or_else(|| machine_id.to_string())
            })
            .collect();
        Self { owner_host_ids }
    }

    pub fn status(&self) -> RoutineRegistryStatus {
        RoutineRegistryStatus {
            source: "local_workspace_registry",
            state: "current",
            age_seconds: None,
            diagnostics: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutinePlacementProjection {
    pub local_host: RoutineHostIdentity,
    pub registry: RoutineRegistryView,
}

pub trait RoutinePlacementProvider {
    fn load_routine_placement(&self) -> Result<RoutinePlacementProjection, OrbitError>;
}

/// Produce the three v1 outcomes using local data only: own-host matches,
/// known owner names belong elsewhere, and unknown names are unresolvable.
pub fn validate_routine_pins(
    identity: &dyn RoutineHostIdentityView,
    origin: RoutineOrigin,
    pins: &[String],
    view: &RoutineRegistryView,
) -> RoutinePinValidation {
    if origin == RoutineOrigin::Local {
        return RoutinePinValidation {
            eligible: true,
            diagnostics: Vec::new(),
        };
    }

    let mut eligible = false;
    let mut diagnostics = Vec::new();
    for pin in pins {
        if pin == identity.host_id() {
            eligible = true;
        } else if view.owner_host_ids.contains(pin) {
            diagnostics.push(diagnostic(
                "host_belongs_elsewhere",
                RoutineDiagnosticSeverity::Warning,
                pin,
                format!("host pin '{pin}' belongs to another owner known locally"),
            ));
        } else {
            diagnostics.push(diagnostic(
                "host_unresolvable",
                RoutineDiagnosticSeverity::Error,
                pin,
                format!(
                    "host pin '{pin}' is not this machine and is not named by any local workspace owner record"
                ),
            ));
        }
    }
    RoutinePinValidation {
        eligible,
        diagnostics,
    }
}

fn diagnostic(
    code: &'static str,
    severity: RoutineDiagnosticSeverity,
    pin: &str,
    message: String,
) -> RoutineValidationDiagnostic {
    RoutineValidationDiagnostic {
        code,
        severity,
        pin: Some(pin.to_string()),
        message,
        stale: false,
    }
}
