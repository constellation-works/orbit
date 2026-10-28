//! Lease listener configuration validation.

use super::{LeaseListenerLimits, tls};
use orbit_auth::client_address::{LeaseClientAddressPolicy, TrustedProxies};
use orbit_config::LeaseListenerSettings;
use orbit_core::OrbitError;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// The listener's validated configuration.
#[derive(Debug, Clone)]
pub struct LeaseListenerConfig {
    /// Address to listen on.
    pub bind: SocketAddr,
    /// Native TLS, validated and loaded before binding; `None` serves HTTP.
    pub tls: Option<Arc<rustls::ServerConfig>>,
    /// How the client address is found.
    pub client_address: LeaseClientAddressPolicy,
    /// The operator's lease intake socket.
    pub intake_socket: PathBuf,
    /// Failed authentications from one address before it gets 429.
    pub failed_auth_limit: u32,
    /// The window those failures are counted over.
    pub failed_auth_window: Duration,
    /// Request, transport and concurrency bounds.
    pub limits: LeaseListenerLimits,
}

impl LeaseListenerConfig {
    /// Validate `lease.listener.*`, refusing an unsafe or incomplete
    /// combination with a message naming the setting.
    pub fn from_settings(settings: &LeaseListenerSettings) -> Result<Self, OrbitError> {
        let tls = settings
            .tls
            .as_ref()
            .map(tls::load_listener_tls)
            .transpose()?;
        let Some(raw_bind) = settings.bind.as_deref() else {
            return Err(refused(
                "lease.listener.bind is unset, so there is no listener. Set it in the listener \
                 user's ~/.orbit/config.toml, for example bind = \"100.64.0.5:7480\"",
            ));
        };
        let bind: SocketAddr = raw_bind.parse().map_err(|_| {
            refused(&format!(
                "lease.listener.bind = \"{raw_bind}\" is not an IP address and port, such as \
                 127.0.0.1:7480 or [::1]:7480"
            ))
        })?;
        let client_address = if settings.behind_proxy {
            if settings.trusted_proxy.is_empty() {
                return Err(refused(
                    "lease.listener.behind_proxy = true needs lease.listener.trusted_proxy: list \
                     the proxy addresses or CIDR ranges whose X-Forwarded-For may be read",
                ));
            }
            LeaseClientAddressPolicy::BehindProxy(TrustedProxies::parse(&settings.trusted_proxy)?)
        } else {
            LeaseClientAddressPolicy::Direct
        };
        if bind.ip().is_unspecified() && tls.is_none() && !settings.behind_proxy {
            return Err(refused(&format!(
                "lease.listener.bind = \"{raw_bind}\" listens on every interface without TLS. \
                 Set lease.listener.tls, bind a specific address, or put a terminating proxy in front and set \
                 lease.listener.behind_proxy = true"
            )));
        }
        let Some(intake_socket) = settings.intake_socket.as_deref() else {
            return Err(refused(
                "lease.listener.intake_socket is unset: name the operator's lease intake socket, \
                 such as /home/<operator>/.orbit/run/lease-intake.sock",
            ));
        };
        let intake_socket = PathBuf::from(intake_socket);
        if !intake_socket.is_absolute() {
            return Err(refused(&format!(
                "lease.listener.intake_socket = \"{}\" must be an absolute path",
                intake_socket.display()
            )));
        }
        Ok(Self {
            bind,
            tls,
            client_address,
            intake_socket,
            failed_auth_limit: settings.failed_auth_limit,
            failed_auth_window: Duration::from_secs(u64::from(settings.failed_auth_window_seconds)),
            limits: LeaseListenerLimits::from_settings(settings),
        })
    }
}

pub(super) fn refused(detail: &str) -> OrbitError {
    OrbitError::InvalidInput(format!("refusing to start the lease listener: {detail}"))
}
