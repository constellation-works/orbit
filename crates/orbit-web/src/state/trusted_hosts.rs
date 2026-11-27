//! The dashboard Host gate's trusted-host policy (`machine.web.trusted_*`),
//! held on `DashboardState` and applied by `api::origin`.

use std::collections::BTreeSet;

use orbit_common::OrbitError;

/// Non-loopback Hosts the dashboard admits, from `machine.web.trusted_hosts`,
/// and the `Tailscale-User-Login` values admitted through them, from
/// `machine.web.trusted_logins`. Empty by default: only loopback Hosts pass.
#[derive(Debug, Default)]
pub(crate) struct TrustedHosts {
    hosts: BTreeSet<String>,
    logins: BTreeSet<String>,
}

impl TrustedHosts {
    /// Build the policy from the resolved keys (hosts already lowercased).
    /// Hosts without logins would admit no one through them while looking
    /// configured, so that is refused, naming the missing key.
    pub(crate) fn new(hosts: Vec<String>, logins: Vec<String>) -> Result<Self, OrbitError> {
        if !hosts.is_empty() && logins.is_empty() {
            return Err(OrbitError::InvalidInput(
                "machine.web.trusted_hosts is set but machine.web.trusted_logins is empty: \
                 list the Tailscale logins allowed through the trusted hosts with \
                 `orbit config set --global machine.web.trusted_logins '[\"<login>\"]'`, \
                 or unset machine.web.trusted_hosts"
                    .to_string(),
            ));
        }
        Ok(Self {
            hosts: hosts.into_iter().collect(),
            logins: logins.into_iter().collect(),
        })
    }

    pub(crate) fn hosts(&self) -> impl Iterator<Item = &str> {
        self.hosts.iter().map(String::as_str)
    }

    /// Whether `name` (lowercase) is a trusted host.
    pub(crate) fn admits_host(&self, name: &str) -> bool {
        self.hosts.contains(name)
    }

    /// Whether `login` is a trusted `Tailscale-User-Login` value.
    pub(crate) fn admits_login(&self, login: &str) -> bool {
        self.logins.contains(login)
    }

    pub(crate) fn logins(&self) -> impl Iterator<Item = &str> {
        self.logins.iter().map(String::as_str)
    }

    /// Whether this policy holds exactly these resolved keys.
    pub(crate) fn matches(&self, hosts: &[String], logins: &[String]) -> bool {
        self.hosts.iter().eq(hosts.iter().collect::<BTreeSet<_>>())
            && self
                .logins
                .iter()
                .eq(logins.iter().collect::<BTreeSet<_>>())
    }

    pub(crate) fn login_count(&self) -> usize {
        self.logins.len()
    }
}
