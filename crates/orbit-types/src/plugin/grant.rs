//! Grants: the operator's answer to a manifest's `permissions` request.
//!
//! A manifest *requests* (`spec.permissions`, `spec.backend.sandbox`); the
//! operator *grants* at `orbit plugin enable --grant …`. Only the second is
//! authority (design §4.1). This module is the one mapping from request to
//! grant name, so the loader, `orbit plugin show`, and the audit row all
//! agree on which grants a plugin needs.

use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};

use super::manifest::{PluginManifest, PluginNetworkPermission, PluginSandbox};

/// One grantable capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginGrant {
    /// `permissions.fs.read` / `permissions.fs.write`: the paths the sandbox
    /// opens for the backend.
    Fs,
    /// `permissions.network: loopback | any`.
    Network,
    /// `permissions.env_pass`: parent variables copied into the child.
    EnvPass,
    /// `permissions.orbit_tools`: callbacks through `orbit tool run`.
    OrbitTools,
    /// `backend.sandbox: none`: the backend runs unconfined.
    Unsandboxed,
}

impl PluginGrant {
    /// Every grant, in the order surfaces list them.
    pub const ALL: [PluginGrant; 5] = [
        PluginGrant::Fs,
        PluginGrant::Network,
        PluginGrant::EnvPass,
        PluginGrant::OrbitTools,
        PluginGrant::Unsandboxed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fs => "fs",
            Self::Network => "network",
            Self::EnvPass => "env_pass",
            Self::OrbitTools => "orbit_tools",
            Self::Unsandboxed => "unsandboxed",
        }
    }

    /// The `--grant` spelling, or `None` for a name no surface knows.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|grant| grant.as_str() == name.trim())
    }

    /// Which manifest key asks for this grant, for a diagnostic.
    pub fn requested_by(self) -> &'static str {
        match self {
            Self::Fs => "spec.permissions.fs",
            Self::Network => "spec.permissions.network",
            Self::EnvPass => "spec.permissions.env_pass",
            Self::OrbitTools => "spec.permissions.orbit_tools",
            Self::Unsandboxed => "spec.backend.sandbox: none",
        }
    }
}

impl Display for PluginGrant {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Validate a `--grant` list: every name must be one of [`PluginGrant::ALL`].
/// Returns the accepted grants deduplicated in canonical order.
pub fn parse_grants(names: &[String]) -> Result<Vec<PluginGrant>, String> {
    let mut unknown = Vec::new();
    let mut grants = Vec::new();
    for name in names {
        match PluginGrant::parse(name) {
            Some(grant) if !grants.contains(&grant) => grants.push(grant),
            Some(_) => {}
            None => unknown.push(name.trim().to_string()),
        }
    }
    if !unknown.is_empty() {
        return Err(format!(
            "unknown grant{} {}; valid grants are {}",
            if unknown.len() == 1 { "" } else { "s" },
            unknown.join(", "),
            PluginGrant::ALL
                .iter()
                .map(|grant| grant.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    grants.sort();
    Ok(grants)
}

/// What one grant means for this manifest: whether the manifest asks for it,
/// and the concrete request behind the ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginGrantRequest {
    pub grant: PluginGrant,
    /// The request as the manifest states it (`read=…, write=…`), empty when
    /// the manifest does not ask for this grant.
    pub requested: Option<String>,
}

impl PluginManifest {
    /// Every grant with the manifest's request beside it, in canonical order.
    pub fn grant_requests(&self) -> Vec<PluginGrantRequest> {
        let permissions = &self.spec.permissions;
        PluginGrant::ALL
            .into_iter()
            .map(|grant| {
                let requested = match grant {
                    PluginGrant::Fs => {
                        let mut parts = Vec::new();
                        if !permissions.fs.read.is_empty() {
                            parts.push(format!("read={}", permissions.fs.read.join(",")));
                        }
                        if !permissions.fs.write.is_empty() {
                            parts.push(format!("write={}", permissions.fs.write.join(",")));
                        }
                        (!parts.is_empty()).then(|| parts.join(" "))
                    }
                    PluginGrant::Network => match permissions.network {
                        PluginNetworkPermission::None => None,
                        PluginNetworkPermission::Loopback => Some("loopback".to_string()),
                        PluginNetworkPermission::Any => Some("any".to_string()),
                    },
                    PluginGrant::EnvPass => {
                        (!permissions.env_pass.is_empty()).then(|| permissions.env_pass.join(","))
                    }
                    PluginGrant::OrbitTools => (!permissions.orbit_tools.is_empty())
                        .then(|| permissions.orbit_tools.join(",")),
                    PluginGrant::Unsandboxed => (self.spec.backend.sandbox == PluginSandbox::None)
                        .then(|| "backend.sandbox: none".to_string()),
                };
                PluginGrantRequest { grant, requested }
            })
            .collect()
    }

    /// The grants this manifest cannot run without.
    pub fn required_grants(&self) -> Vec<PluginGrant> {
        self.grant_requests()
            .into_iter()
            .filter(|request| request.requested.is_some())
            .map(|request| request.grant)
            .collect()
    }

    /// Required grants the operator has not recorded, in canonical order.
    pub fn missing_grants(&self, granted: &[String]) -> Vec<PluginGrant> {
        self.required_grants()
            .into_iter()
            .filter(|grant| !granted.iter().any(|name| name == grant.as_str()))
            .collect()
    }
}
