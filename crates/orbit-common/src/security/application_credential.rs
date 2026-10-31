//! Application secrets are minted once, hashed at rest, and passed by fd.

#[cfg(unix)]
use std::fs::File;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::OrbitError;

/// Descriptor number, never a credential value. Only local CLI calls read it.
pub const APPLICATION_CREDENTIAL_FD_ENV: &str = "ORBIT_APPLICATION_CREDENTIAL_FD";

/// A one-time mint result. Deliberately neither serializable nor cloneable.
pub struct ApplicationCredential {
    id: String,
    value: String,
}

impl std::fmt::Debug for ApplicationCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApplicationCredential")
            .finish_non_exhaustive()
    }
}

impl ApplicationCredential {
    /// Generate an independent public consent id and 256-bit secret.
    pub fn mint() -> Result<Self, OrbitError> {
        let mut bytes = [0u8; 48];
        getrandom::fill(&mut bytes).map_err(|_| {
            OrbitError::Execution("application credential entropy unavailable".into())
        })?;
        let hex = |bytes: &[u8]| {
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        let id = hex(&bytes[..16]);
        let value = format!("{id}.{}", hex(&bytes[16..]));
        Ok(Self { id, value })
    }

    pub fn consent_id(&self) -> &str {
        &self.id
    }

    /// Only this digest is persisted by the host.
    pub fn fingerprint(&self) -> String {
        orbit_types::identity::lower_hex(Sha256::digest(self.value.as_bytes()))
    }

    /// Whether `stored` is this credential's fingerprint, compared in time
    /// independent of where the strings first differ.
    pub fn matches_fingerprint(&self, stored: &str) -> bool {
        let actual = self.fingerprint();
        actual.len() == stored.len()
            && actual
                .bytes()
                .zip(stored.bytes())
                .fold(0u8, |diff, (a, b)| diff | (a ^ b))
                == 0
    }

    /// Consume the mint result into a new owner-only credential file. Never
    /// overwrites an existing file or prints the credential.
    pub fn provision(self, path: &Path) -> Result<(), OrbitError> {
        if !cfg!(unix) {
            return Err(OrbitError::CapabilityDenied(
                "application credential provisioning requires Unix file permissions".into(),
            ));
        }
        crate::fs::io::write_new_private_text(path, &self.value)?;
        Ok(())
    }

    /// Read a bounded regular file through an inherited descriptor without
    /// changing its shared offset. Pipes and invalid descriptors fail closed.
    pub fn from_process_descriptor() -> Result<Option<Self>, OrbitError> {
        let Some(raw) = std::env::var_os(APPLICATION_CREDENTIAL_FD_ENV) else {
            return Ok(None);
        };
        let refused =
            || OrbitError::CapabilityDenied("invalid application credential descriptor".into());
        let fd = raw
            .to_str()
            .and_then(|s| s.parse::<i32>().ok())
            .filter(|fd| *fd > 2)
            .ok_or_else(refused)?;
        #[cfg(not(unix))]
        {
            let _ = fd;
            Err(refused())
        }
        #[cfg(unix)]
        {
            use std::os::fd::FromRawFd;
            use std::os::unix::fs::FileExt;
            // SAFETY: ManuallyDrop borrows the inherited descriptor; no close.
            let file = std::mem::ManuallyDrop::new(unsafe { File::from_raw_fd(fd) });
            // Do not pass the service's raw credential to a plugin backend.
            // SAFETY: fcntl inspects/updates flags of this borrowed descriptor.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0
            {
                return Err(refused());
            }
            let metadata = file.metadata().map_err(|_| refused())?;
            use std::os::unix::fs::MetadataExt;
            // SAFETY: geteuid has no memory safety preconditions.
            if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
                return Err(refused());
            }
            if !metadata.is_file() || metadata.len() != 97 {
                return Err(refused());
            }
            let mut bytes = [0u8; 97];
            file.read_exact_at(&mut bytes, 0).map_err(|_| refused())?;
            let value = String::from_utf8(bytes.to_vec()).map_err(|_| refused())?;
            if bytes[32] != b'.'
                || !bytes
                    .iter()
                    .enumerate()
                    .all(|(i, b)| i == 32 || b.is_ascii_hexdigit())
            {
                return Err(refused());
            }
            Ok(Some(Self {
                id: value[..32].to_string(),
                value,
            }))
        }
    }
}
