//! The lease listener's settings, read alone from the listener user's global
//! `config.toml`.

use std::path::Path;

use orbit_common::OrbitError;
use serde::{Deserialize, Serialize};

use super::resolve::{
    read_optional, resolve_at, resolve_bounded_count, resolve_bounded_range,
    resolve_optional_non_empty,
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

/// PEM certificate chain and private key for native lease-listener TLS.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseListenerTlsSettings {
    /// Certificate chain path, with the leaf certificate first.
    pub cert: String,
    /// Matching unencrypted private key path.
    pub key: String,
}

pub(super) fn resolve_lease_listener_tls(
    raw: Option<LeaseListenerTlsSettings>,
) -> Result<Option<LeaseListenerTlsSettings>, OrbitError> {
    if let Some(tls) = &raw
        && (tls.cert.trim().is_empty() || tls.key.trim().is_empty())
    {
        return Err(OrbitError::InvalidInput(format!(
            "{LEASE_LISTENER_TLS_KEY} requires non-empty cert and key paths"
        )));
    }
    Ok(raw)
}

/// `lease.listener.*` as written; `orbit lease listen` checks addresses,
/// ranges and the combination at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseListenerSettings {
    /// `lease.listener.bind`: the address to listen on, or `None` for no
    /// listener.
    pub bind: Option<String>,
    /// Native TLS material, loaded and checked by the listener at startup.
    pub tls: Option<LeaseListenerTlsSettings>,
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
    /// Requests per minute from one resolved address or IPv6 prefix.
    pub address_per_minute: u32,
    /// IPv6 prefix length for address budgets; mapped IPv4 remains per address.
    pub ipv6_prefix_len: u32,
    /// Read requests per minute per token, independent of writes and address.
    pub token_reads_per_minute: u32,
    /// Write requests per minute per token, independent of reads and address.
    pub token_writes_per_minute: u32,
    /// Open notice streams per token; streams also count as in-flight requests.
    pub max_streams_per_token: u32,
    /// In-flight requests per token, including open streams.
    pub max_in_flight_per_token: u32,
    /// Concurrent listener connections, including TLS handshakes.
    pub max_connections: u32,
    /// Concurrent listener requests; excess receives 503.
    pub max_in_flight_requests: u32,
    /// Maximum request body bytes; oversized requests receive 413.
    pub max_body_bytes: u32,
    /// Seconds allowed to receive HTTP headers or complete a TLS handshake.
    pub header_timeout_seconds: u32,
    /// Seconds allowed for a request handler and body receipt.
    pub request_timeout_seconds: u32,
    /// Maximum rate-limit identities, including two shared overflow buckets.
    pub max_tracked_keys: u32,
}

pub(super) fn resolve_listener_address_per_minute(raw: Option<u32>) -> Result<u32, OrbitError> {
    resolve_bounded_range(
        raw,
        120,
        1,
        100000,
        "lease.listener.limits.address_per_minute",
    )
}

pub(super) fn resolve_listener_ipv6_prefix_len(raw: Option<u32>) -> Result<u32, OrbitError> {
    resolve_bounded_range(raw, 64, 0, 128, "lease.listener.limits.ipv6_prefix_len")
}

pub(super) fn resolve_listener_token_reads_per_minute(raw: Option<u32>) -> Result<u32, OrbitError> {
    resolve_bounded_range(
        raw,
        60,
        1,
        100000,
        "lease.listener.limits.token_reads_per_minute",
    )
}

pub(super) fn resolve_listener_token_writes_per_minute(
    raw: Option<u32>,
) -> Result<u32, OrbitError> {
    resolve_bounded_range(
        raw,
        20,
        1,
        100000,
        "lease.listener.limits.token_writes_per_minute",
    )
}

pub(super) fn resolve_listener_max_streams_per_token(raw: Option<u32>) -> Result<u32, OrbitError> {
    resolve_bounded_range(
        raw,
        2,
        1,
        100,
        "lease.listener.limits.max_streams_per_token",
    )
}

pub(super) fn resolve_listener_max_in_flight_per_token(
    raw: Option<u32>,
) -> Result<u32, OrbitError> {
    resolve_bounded_range(
        raw,
        8,
        1,
        1000,
        "lease.listener.limits.max_in_flight_per_token",
    )
}

pub(super) fn resolve_listener_max_connections(raw: Option<u32>) -> Result<u32, OrbitError> {
    resolve_bounded_range(raw, 64, 1, 10000, "lease.listener.limits.max_connections")
}

pub(super) fn resolve_listener_max_in_flight_requests(raw: Option<u32>) -> Result<u32, OrbitError> {
    resolve_bounded_range(
        raw,
        16,
        1,
        1000,
        "lease.listener.limits.max_in_flight_requests",
    )
}

pub(super) fn resolve_listener_max_body_bytes(raw: Option<u32>) -> Result<u32, OrbitError> {
    resolve_bounded_range(
        raw,
        65536,
        1,
        1048576,
        "lease.listener.limits.max_body_bytes",
    )
}

pub(super) fn resolve_listener_header_timeout_seconds(raw: Option<u32>) -> Result<u32, OrbitError> {
    resolve_bounded_range(
        raw,
        5,
        1,
        300,
        "lease.listener.limits.header_timeout_seconds",
    )
}

pub(super) fn resolve_listener_request_timeout_seconds(
    raw: Option<u32>,
) -> Result<u32, OrbitError> {
    resolve_bounded_range(
        raw,
        10,
        1,
        300,
        "lease.listener.limits.request_timeout_seconds",
    )
}

pub(super) fn resolve_listener_max_tracked_keys(raw: Option<u32>) -> Result<u32, OrbitError> {
    resolve_bounded_range(
        raw,
        4096,
        3,
        65536,
        "lease.listener.limits.max_tracked_keys",
    )
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
            tls: resolve_lease_listener_tls(read_optional(
                document,
                LEASE_LISTENER_TLS_KEY,
                config_path,
            )?)?,
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
            address_per_minute: resolve_listener_address_per_minute(read_optional(
                document,
                "lease.listener.limits.address_per_minute",
                config_path,
            )?)?,
            ipv6_prefix_len: resolve_listener_ipv6_prefix_len(read_optional(
                document,
                "lease.listener.limits.ipv6_prefix_len",
                config_path,
            )?)?,
            token_reads_per_minute: resolve_listener_token_reads_per_minute(read_optional(
                document,
                "lease.listener.limits.token_reads_per_minute",
                config_path,
            )?)?,
            token_writes_per_minute: resolve_listener_token_writes_per_minute(read_optional(
                document,
                "lease.listener.limits.token_writes_per_minute",
                config_path,
            )?)?,
            max_streams_per_token: resolve_listener_max_streams_per_token(read_optional(
                document,
                "lease.listener.limits.max_streams_per_token",
                config_path,
            )?)?,
            max_in_flight_per_token: resolve_listener_max_in_flight_per_token(read_optional(
                document,
                "lease.listener.limits.max_in_flight_per_token",
                config_path,
            )?)?,
            max_connections: resolve_listener_max_connections(read_optional(
                document,
                "lease.listener.limits.max_connections",
                config_path,
            )?)?,
            max_in_flight_requests: resolve_listener_max_in_flight_requests(read_optional(
                document,
                "lease.listener.limits.max_in_flight_requests",
                config_path,
            )?)?,
            max_body_bytes: resolve_listener_max_body_bytes(read_optional(
                document,
                "lease.listener.limits.max_body_bytes",
                config_path,
            )?)?,
            header_timeout_seconds: resolve_listener_header_timeout_seconds(read_optional(
                document,
                "lease.listener.limits.header_timeout_seconds",
                config_path,
            )?)?,
            request_timeout_seconds: resolve_listener_request_timeout_seconds(read_optional(
                document,
                "lease.listener.limits.request_timeout_seconds",
                config_path,
            )?)?,
            max_tracked_keys: resolve_listener_max_tracked_keys(read_optional(
                document,
                "lease.listener.limits.max_tracked_keys",
                config_path,
            )?)?,
        })
    })();
    resolve_at(settings, config_path)
}
