//! Lease intake protocol version, frame bounds and handshake diagnostics.

/// The intake protocol version. A listener and an Orbit Web of different
/// versions refuse each other, since the listener's root-owned binary is
/// updated separately from the operator's. Any change to the frames or
/// operations changes it. Version 2 added `notices` and `events`.
pub const LEASE_INTAKE_PROTOCOL_VERSION: u32 = 2;

/// The socket's path under the operator's Orbit home.
pub const LEASE_INTAKE_SOCKET_PATH: &str = "run/lease-intake.sock";

/// Largest frame body either side accepts.
pub const LEASE_INTAKE_MAX_FRAME_BYTES: u32 = 64 * 1024;

/// The refusal both sides give for a protocol version other than their own.
pub(super) fn version_mismatch_message(
    local: &str,
    local_version: u32,
    remote: &str,
    remote_version: u32,
) -> String {
    format!(
        "lease intake protocol mismatch: {local} speaks version {local_version} and {remote} \
         speaks version {remote_version}; install the same Orbit release for both"
    )
}
