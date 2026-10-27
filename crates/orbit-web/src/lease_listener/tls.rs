//! Startup-only TLS material and bounded concurrent handshakes. HTTP routing,
//! peer attribution and graceful shutdown remain Axum's responsibility.

use std::convert::Infallible;
use std::fs::File;
use std::io;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ConnectInfo;
use axum::serve::{IncomingStream, Listener};
use axum::{Extension, Router};
use orbit_config::LeaseListenerTlsSettings;
use orbit_core::OrbitError;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio_rustls::{TlsAcceptor, server::TlsStream};

use super::refused;

const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TLS_HANDSHAKES: usize = 64;

pub(super) fn load_listener_tls(
    settings: &LeaseListenerTlsSettings,
) -> Result<Arc<rustls::ServerConfig>, OrbitError> {
    let invalid = |detail| refused(&format!("lease.listener.tls: {detail}"));
    let cert_file = File::open(&settings.cert)
        .map_err(|error| invalid(format!("cannot open cert {}: {error}", settings.cert)))?;
    let key_file = File::open(&settings.key)
        .map_err(|error| invalid(format!("cannot open key {}: {error}", settings.key)))?;
    // Check the descriptor we read, so replacing the path cannot swap the
    // checked permissions for a different key's permissions.
    let metadata = key_file
        .metadata()
        .map_err(|error| invalid(format!("cannot inspect key {}: {error}", settings.key)))?;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(invalid(format!(
            "key {} has group or other permissions; restrict it to 0600",
            settings.key
        )));
    }
    let certificates = CertificateDer::pem_reader_iter(cert_file)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| invalid(format!("cannot read cert {}: {error}", settings.cert)))?;
    let key = PrivateKeyDer::from_pem_reader(key_file)
        .map_err(|error| invalid(format!("cannot read key {}: {error}", settings.key)))?;
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
    .map_err(|error| invalid(format!("cannot configure TLS 1.2/1.3: {error}")))?
    .with_no_client_auth()
    // rustls checks both the certificate encoding and the public/private
    // key match. No TLS listener is bound if either check fails.
    .with_single_cert(certificates, key)
    .map_err(|error| invalid(format!("invalid cert/key pair: {error}")))?;
    Ok(Arc::new(config))
}

pub(super) async fn serve_tls_listener(
    listener: TcpListener,
    config: Arc<rustls::ServerConfig>,
    app: Router,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    let listener = LeaseTlsListener {
        listener,
        acceptor: TlsAcceptor::from(config),
        handshakes: JoinSet::new(),
    };
    let make_service = tower::service_fn(move |stream: IncomingStream<'_, LeaseTlsListener>| {
        let app = app
            .clone()
            .layer(Extension(ConnectInfo(*stream.remote_addr())));
        std::future::ready(Ok::<_, Infallible>(app))
    });
    axum::serve(listener, make_service)
        .with_graceful_shutdown(shutdown)
        .await
}

type HandshakeResult = io::Result<(TlsStream<TcpStream>, SocketAddr)>;

struct LeaseTlsListener {
    listener: TcpListener,
    acceptor: TlsAcceptor,
    // Dropping the listener on shutdown also aborts pending handshakes.
    handshakes: JoinSet<HandshakeResult>,
}

impl Listener for LeaseTlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            tokio::select! {
                result = self.handshakes.join_next(), if !self.handshakes.is_empty() => {
                    match result {
                        Some(Ok(Ok(accepted))) => return accepted,
                        Some(Ok(Err(error))) => tracing::debug!(%error, "lease TLS handshake refused"),
                        Some(Err(error)) => tracing::warn!(%error, "lease TLS handshake task failed"),
                        None => {}
                    }
                }
                // Axum retries TCP accept errors with its normal backoff.
                (stream, peer) = Listener::accept(&mut self.listener),
                    if self.handshakes.len() < MAX_TLS_HANDSHAKES => {
                    let acceptor = self.acceptor.clone();
                    self.handshakes.spawn(async move {
                        let stream = tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(stream))
                            .await
                            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake timed out"))??;
                        Ok((stream, peer))
                    });
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}
