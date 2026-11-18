//! SIGINT and SIGTERM future shared by the dashboard server and the lease listener.

use std::future::Future;

use orbit_core::OrbitError;

/// Prepare shutdown notification, installing Unix handlers before returning.
pub(crate) fn shutdown_signal() -> Result<impl Future<Output = ()> + Send, OrbitError> {
    #[cfg(unix)]
    let mut interrupt =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .map_err(|error| OrbitError::Execution(format!("install SIGINT handler: {error}")))?;
    #[cfg(unix)]
    let ctrl_c = async move {
        interrupt.recv().await;
    };

    #[cfg(not(unix))]
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|error| OrbitError::Execution(format!("install SIGTERM handler: {error}")))?;
    #[cfg(unix)]
    let terminate = async move {
        sigterm.recv().await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    Ok(async move {
        tokio::select! {
            _ = ctrl_c => {}
            _ = terminate => {}
        }
    })
}
