//! Lease listener OS-user checks and server startup.

use super::config::refused;
use super::router::build_listener_router;
use super::{LeaseListenerConfig, LeaseListenerSettings, transport};
use orbit_auth::environment::{LISTENER_USER, check_listener_execution_environment};
use orbit_auth::store::LeaseTokenStore;
use orbit_core::OrbitError;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// Check the execution environment and the intake socket's owner, validate
/// the listener settings, open the running user's token store, and serve
/// until SIGINT or SIGTERM.
///
/// Path leases are not configurable [ORB-22808]: no command runs this, and
/// the unconfigured [`LeaseListenerSettings::default`] it validates names no
/// bind address, so it refuses to start.
pub fn run_lease_listener() -> Result<(), OrbitError> {
    let orbit_home = orbit_common::fs::path::global_orbit_dir()?;
    check_listener_execution_environment(&orbit_home)?;
    let config = LeaseListenerConfig::from_settings(&LeaseListenerSettings::default())?;
    refuse_intake_socket_owner(&config.intake_socket)?;
    let store = LeaseTokenStore::open_in_orbit_home(&orbit_home)?;
    serve_lease_listener(config, store)
}

/// Refuse to run as the user that owns the intake socket: that is the
/// operator, whose agents must not run the listener. When the socket is not
/// there yet, its directory's owner is checked instead.
fn refuse_intake_socket_owner(socket: &Path) -> Result<(), OrbitError> {
    let (inspected, metadata) = match std::fs::metadata(socket) {
        Ok(metadata) => (socket, metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let directory = socket.parent().unwrap_or(socket);
            let metadata = std::fs::metadata(directory).map_err(|error| {
                refused(&format!(
                    "cannot inspect the lease intake socket {} or its directory: {error}. Name \
                     the socket Orbit Web serves to the lease listener user",
                    socket.display()
                ))
            })?;
            (directory, metadata)
        }
        Err(error) => {
            return Err(refused(&format!(
                "cannot inspect the lease intake socket {}: {error}",
                socket.display()
            )));
        }
    };
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    if metadata.uid() == euid {
        return Err(OrbitError::PolicyDenied(format!(
            "refusing to start the lease listener: {} is owned by the running user (uid {euid}), \
             so this is the operator's user. Run the listener as the listener user \
             ({LISTENER_USER} by convention), for example from its systemd unit",
            inspected.display()
        )));
    }
    Ok(())
}

/// Bind `config.bind` and serve the listener router until SIGINT or SIGTERM.
pub fn serve_lease_listener(
    config: LeaseListenerConfig,
    store: LeaseTokenStore,
) -> Result<(), OrbitError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| OrbitError::Execution(format!("lease listener runtime: {error}")))?;
    runtime.block_on(async move {
        let shutdown = crate::serve::shutdown_signal()?;
        let listener = tokio::net::TcpListener::bind(config.bind)
            .await
            .map_err(|error| {
                OrbitError::Io(format!(
                    "lease listener cannot bind {}: {error}",
                    config.bind
                ))
            })?;
        let local = listener
            .local_addr()
            .map_err(|error| OrbitError::Io(format!("lease listener: {error}")))?;
        tracing::info!(address = %local, intake = %config.intake_socket.display(), "serving the lease listener");
        #[allow(
            clippy::print_stdout,
            reason = "the operator reads the bound lease URL from stdout because the listener has no other status channel"
        )]
        {
            let scheme = if config.tls.is_some() { "https" } else { "http" };
            println!("Lease listener on {scheme}://{local} (lease routes and /healthz only)");
        }
        serve_lease_listener_on(listener, config, store, shutdown).await
    })
}

/// Serve an already bound socket with the same transport, router and peer
/// addresses as [`serve_lease_listener`], until `shutdown` completes.
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
