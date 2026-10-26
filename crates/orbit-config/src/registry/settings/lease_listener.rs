//! The lease listener's settings, read alone from the listener user's global
//! `config.toml`.

use std::path::Path;

use orbit_common::OrbitError;

use super::resolve::{
    read_optional, resolve_at, resolve_bounded_count, resolve_optional_non_empty,
};

pub(super) const LEASE_LISTENER_BIND_KEY: &str = "lease.listener.bind";
pub(super) const LEASE_LISTENER_TLS_KEY: &str = "lease.listener.tls";
pub(super) const LEASE_LISTENER_BEHIND_PROXY_KEY: &str = "lease.listener.behind_proxy";
pub(super) const LEASE_LISTENER_TRUSTED_PROXY_KEY: &str = "lease.listener.trusted_proxy";
pub(super) const LEASE_LISTENER_INTAKE_SOCKET_KEY: &str = "lease.listener.intake_socket";
pub(super) const LEASE_LISTENER_FAILED_AUTH_KEY: &str = "lease.listener.limits.failed_auth";
pub(super) const LEASE_LISTENER_FAILED_AUTH_WINDOW_KEY: &str =
    "lease.listener.limits.failed_auth_window_seconds";

/// Failed authentications from one address before it is refused.
const DEFAULT_FAILED_AUTH_LIMIT: u32 = 10;
/// The failed-authentication window, in seconds.
const DEFAULT_FAILED_AUTH_WINDOW_SECONDS: u32 = 300;

/// `lease.listener.*` as written; `orbit lease listen` checks addresses,
/// ranges and the combination at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseListenerSettings {
    /// `lease.listener.bind`: the address to listen on, or `None` for no
    /// listener.
    pub bind: Option<String>,
    /// Whether `lease.listener.tls` is set at all, in any form.
    pub tls_configured: bool,
    /// `lease.listener.behind_proxy`.
    pub behind_proxy: bool,
    /// `lease.listener.trusted_proxy`: addresses or CIDR ranges.
    pub trusted_proxy: Vec<String>,
    /// `lease.listener.intake_socket`: the operator's intake socket.
    pub intake_socket: Option<String>,
    /// `lease.listener.limits.failed_auth`.
    pub failed_auth_limit: u32,
    /// `lease.listener.limits.failed_auth_window_seconds`.
    pub failed_auth_window_seconds: u32,
}

/// `lease.listener.trusted_proxy`: trimmed, non-empty entries.
pub(super) fn resolve_lease_listener_trusted_proxy(
    raw: Option<Vec<String>>,
) -> Result<Vec<String>, OrbitError> {
    raw.unwrap_or_default()
        .into_iter()
        .map(|entry| {
            let entry = entry.trim().to_string();
            if entry.is_empty() {
                Err(OrbitError::InvalidInput(format!(
                    "{LEASE_LISTENER_TRUSTED_PROXY_KEY} must not contain an empty entry"
                )))
            } else {
                Ok(entry)
            }
        })
        .collect()
}

pub(super) fn resolve_lease_listener_failed_auth(raw: Option<u32>) -> Result<u32, OrbitError> {
    resolve_bounded_count(
        raw,
        DEFAULT_FAILED_AUTH_LIMIT,
        1000,
        LEASE_LISTENER_FAILED_AUTH_KEY,
    )
}

pub(super) fn resolve_lease_listener_failed_auth_window(
    raw: Option<u32>,
) -> Result<u32, OrbitError> {
    resolve_bounded_count(
        raw,
        DEFAULT_FAILED_AUTH_WINDOW_SECONDS,
        86_400,
        LEASE_LISTENER_FAILED_AUTH_WINDOW_KEY,
    )
}

/// Admit `lease.listener.*` alone from one `config.toml` document.
pub(crate) fn admit_lease_listener_settings(
    document: &toml::Value,
    config_path: &Path,
) -> Result<LeaseListenerSettings, OrbitError> {
    let settings = (|| {
        Ok(LeaseListenerSettings {
            bind: resolve_optional_non_empty(
                read_optional(document, LEASE_LISTENER_BIND_KEY, config_path)?,
                LEASE_LISTENER_BIND_KEY,
            )?,
            tls_configured: read_optional::<toml::Value>(
                document,
                LEASE_LISTENER_TLS_KEY,
                config_path,
            )?
            .is_some(),
            behind_proxy: read_optional(document, LEASE_LISTENER_BEHIND_PROXY_KEY, config_path)?
                .unwrap_or(false),
            trusted_proxy: resolve_lease_listener_trusted_proxy(read_optional(
                document,
                LEASE_LISTENER_TRUSTED_PROXY_KEY,
                config_path,
            )?)?,
            intake_socket: resolve_optional_non_empty(
                read_optional(document, LEASE_LISTENER_INTAKE_SOCKET_KEY, config_path)?,
                LEASE_LISTENER_INTAKE_SOCKET_KEY,
            )?,
            failed_auth_limit: resolve_lease_listener_failed_auth(read_optional(
                document,
                LEASE_LISTENER_FAILED_AUTH_KEY,
                config_path,
            )?)?,
            failed_auth_window_seconds: resolve_lease_listener_failed_auth_window(read_optional(
                document,
                LEASE_LISTENER_FAILED_AUTH_WINDOW_KEY,
                config_path,
            )?)?,
        })
    })();
    resolve_at(settings, config_path)
}
