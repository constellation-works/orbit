//! Lease listener server startup.

use super::router::build_listener_router;
use super::{LeaseListenerConfig, transport};
use orbit_auth::store::LeaseTokenStore;
use orbit_common::OrbitError;

/// Serve an already bound socket with the listener transport, router and peer
/// addresses, until `shutdown` completes.
pub async fn serve_lease_listener_on(
    listener: tokio::net::TcpListener,
    config: LeaseListenerConfig,
    store: LeaseTokenStore,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), OrbitError> {
    let (app, gate) = build_listener_router(&config, store);
    transport::serve_bounded_listener(listener, config, app, gate, shutdown)
        .await
        .map_err(|error| OrbitError::Io(format!("lease listener: {error}")))
}
