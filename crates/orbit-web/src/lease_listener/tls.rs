//! Startup-only TLS material; the shared transport bounds handshakes and HTTP.

use std::fs::File;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use super::refused;
use orbit_config::LeaseListenerTlsSettings;
use orbit_core::OrbitError;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};

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
