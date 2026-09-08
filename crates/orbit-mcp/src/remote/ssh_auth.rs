//! What sshd told this destination about the session it started [ORB-11053].
//!
//! Tier 1 of destination-side caller authorization keys on a label the caller
//! chose, so it can only be an accident guard. This module supplies the
//! missing half: the caller identity is written by the *destination*, next to
//! a public key, in a root-managed `AuthorizedKeysFile`, and sshd will not run
//! that forced command for anyone who cannot complete the key exchange. A
//! destination-issued bearer capability arrives through an sshd-set key
//! environment rather than argv. The dedicated account uses a protected
//! setgid Orbit copy as its login shell, so Linux makes that initial environment unreadable as
//! part of exec, before any userspace startup; Orbit persists only the digest.
//!
//! Three things live here, and nothing else:
//!
//! 1. The `SHA256:` fingerprint of an SSH public key, in the form `ssh-keygen`
//!    and `sshd` print, so an operator can compare what Orbit says with what
//!    their own tools say without a conversion step.
//! 2. Destination-issued acceptance capabilities and optional observation of
//!    the key that authenticated a Tier 1 session.
//! 3. The `authorized_keys` line an operator installs. Orbit renders it and
//!    never writes it: that file governs shell access to the whole machine,
//!    which is far more than Orbit's business.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::{
    STANDARD as BASE64, STANDARD_NO_PAD as BASE64_NO_PAD, URL_SAFE_NO_PAD as BASE64_URL,
};
use orbit_common::OrbitError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Set by sshd to a file listing the authentication methods that succeeded,
/// when the destination's `sshd_config` has `ExposeAuthInfo yes`.
const SSH_USER_AUTH_ENV: &str = "SSH_USER_AUTH";

/// Key-option environment variable carrying the Tier 2 acceptance bearer.
///
/// This name is public because the CLI verifies the kernel launch boundary
/// before it reads the value. It is deliberately not a general configuration
/// input: only [`SshAcceptance::ForcedCommand`] consumes it.
pub const SSH_ACCEPTANCE_ENV: &str = "ORBIT_MCP_SSH_ACCEPTANCE";

/// The command the *caller* asked for, which a forced command replaces.
///
/// Named here only so the one place that mentions it can say, in one spot,
/// that it is never read as input. See [`ignored_original_command`].
const SSH_ORIGINAL_COMMAND_ENV: &str = "SSH_ORIGINAL_COMMAND";

/// Destination-local capabilities issued into generated forced commands.
const SSH_ACCEPTANCE_DIR: &str = "mcp-ssh-acceptance";

/// What every issued capability starts with, so an operator inspecting their
/// root-managed SSH configuration can tell which field Orbit minted.
const ACCEPTANCE_TOKEN_PREFIX: &str = ".orbit-ssh-";

/// Bytes of operating-system entropy behind one acceptance capability.
///
/// 256 bits, comfortably past the 128 a bearer capability needs, because the
/// value is minted once per operator setup and never sits in a hot path.
const ACCEPTANCE_TOKEN_BYTES: usize = 32;

/// The prefix sshd and `ssh-keygen -l` use for a SHA-256 key fingerprint.
pub const FINGERPRINT_PREFIX: &str = "SHA256:";

/// The restrictions that ship with a rendered `authorized_keys` line.
///
/// A forced command alone still leaves the key able to forward ports and
/// allocate a PTY. The MCP transport needs none of that — it is one non-PTY
/// stdio pipe — so the line closes what it does not use. `no-pty` is also what
/// keeps the Tier 1 non-terminal check honest for anyone who reasons about
/// this key without reading the forced command.
pub const FORCED_COMMAND_RESTRICTIONS: &str =
    "no-pty,no-port-forwarding,no-agent-forwarding,no-X11-forwarding";

/// Whether the caller asked for a command that the forced command replaced.
///
/// The value is deliberately not returned, parsed, merged, or used to derive a
/// requested authority: under a forced command the destination composes its
/// own argv, and honoring any part of the caller's string would hand the
/// authority decision straight back to the caller. Only its *presence* is
/// reported, so the audit trail can show that something was overridden.
pub fn ignored_original_command() -> bool {
    std::env::var_os(SSH_ORIGINAL_COMMAND_ENV).is_some()
}

/// How this server process was started, as far as SSH is concerned.
///
/// The two variants are the two tiers. The distinction is a type rather than a
/// pair of flags because the rule it encodes — an identity is honored only
/// when the *destination* composed the argv that carries it — is exactly the
/// rule a `--caller` on an ordinary `orbit mcp serve` would break. Making that
/// combination unrepresentable is cheaper than checking for it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SshAcceptance {
    /// An ordinary `orbit mcp serve`. Whether the session is remote-originated
    /// is the destination's own environment observation, and the caller
    /// identity is at best a label the caller forwarded.
    #[default]
    Environment,
    /// A forced command in the destination's own `authorized_keys` ran this
    /// server, so sshd authenticated the key that selected it.
    ForcedCommand {
        /// The caller identity the destination wrote next to that key.
        caller: Option<String>,
        /// An unguessable capability issued by this destination alongside the
        /// generated `authorized_keys` line.
        acceptance_token: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
struct SshAcceptanceRecord {
    schema_version: u32,
    machine_id: String,
    key_fingerprint: String,
    token_sha256: String,
}

/// An SSH public key as `authorized_keys` and `*.pub` files spell it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshPublicKey {
    /// Key algorithm, such as `ssh-ed25519`.
    pub algorithm: String,
    /// Base64 key blob.
    pub blob: String,
    /// Free-form trailing comment, conventionally `user@host`.
    pub comment: Option<String>,
}

impl SshPublicKey {
    /// The key's fingerprint, in the `SHA256:` form sshd and `ssh-keygen -l`
    /// print.
    pub fn fingerprint(&self) -> Result<String, OrbitError> {
        let blob = BASE64.decode(self.blob.as_bytes()).map_err(|error| {
            OrbitError::InvalidInput(format!("SSH public key is not valid base64: {error}"))
        })?;
        Ok(fingerprint_of_blob(&blob))
    }

    /// The `authorized_keys` line that pins `machine_id` to this key.
    ///
    /// `orbit_command` is the protected login shell's absolute path. sshd
    /// passes the forced command to that shell with `-c`, and the shell uses
    /// the absolute field to recognize its own generated command without a
    /// second exec. The acceptance bearer is installed with sshd's per-key
    /// `environment` option, never in the forced command argv. The destination
    /// requests operator so the matched callers-file grant can be realized;
    /// that grant remains the ceiling and may still cap or deny it.
    pub fn authorized_keys_line(
        &self,
        orbit_command: &str,
        machine_id: &str,
        acceptance_token: &str,
    ) -> String {
        let comment = self
            .comment
            .as_deref()
            .map(|comment| format!(" {comment}"))
            .unwrap_or_default();
        format!(
            "environment=\"{SSH_ACCEPTANCE_ENV}={acceptance_token}\",\
             command=\"{orbit_command} mcp serve --accept-ssh --caller {machine_id} \
             --operator\",{FORCED_COMMAND_RESTRICTIONS} {algorithm} {blob}{comment}",
            algorithm = self.algorithm,
            blob = self.blob,
        )
    }
}

/// Draw one bearer capability from the operating system's CSPRNG.
///
/// The entropy source *is* the security property [ORB-11065]: this token is
/// the only thing standing between a caller who can run an ordinary remote
/// command on this destination and a forged key-bound identity, so every bit
/// of it has to be unpredictable to that caller. `getrandom` reads the OS
/// CSPRNG — `getrandom(2)`, `arc4random_buf`, `BCryptGenRandom` — so no part
/// of the value is a function of state the caller can observe or approximate,
/// such as a wall clock, a process or thread id, or a filename counter. A
/// name generator borrowed from a temporary-file library is not a substitute:
/// those are seeded for collision avoidance, not for secrecy.
///
/// The URL-safe alphabet is chosen over the standard one so the rendered value
/// needs no escaping inside an `authorized_keys` environment option.
fn mint_acceptance_token() -> Result<String, OrbitError> {
    let mut entropy = [0u8; ACCEPTANCE_TOKEN_BYTES];
    getrandom::fill(&mut entropy).map_err(|error| {
        OrbitError::Io(format!(
            "failed to draw operating-system entropy for an SSH acceptance token: {error}"
        ))
    })?;
    Ok(format!(
        "{ACCEPTANCE_TOKEN_PREFIX}{}",
        BASE64_URL.encode(entropy)
    ))
}

/// Issue the destination-only capability embedded in one generated
/// `authorized_keys` key environment. Only its digest is persisted; the bearer
/// value exists solely in the line the operator installs.
pub fn issue_ssh_acceptance(
    global_root: &Path,
    machine_id: &str,
    key_fingerprint: &str,
) -> Result<String, OrbitError> {
    let path = acceptance_record_path(global_root, machine_id);
    let token = mint_acceptance_token()?;

    let record = SshAcceptanceRecord {
        schema_version: 1,
        machine_id: machine_id.to_string(),
        key_fingerprint: key_fingerprint.to_string(),
        token_sha256: BASE64_NO_PAD.encode(Sha256::digest(token.as_bytes())),
    };
    let contents = toml::to_string(&record).map_err(|error| {
        OrbitError::Io(format!("failed to encode SSH acceptance record: {error}"))
    })?;
    orbit_common::fs::io::atomic_write_text(&path, &contents).map_err(|error| {
        OrbitError::Io(format!(
            "failed to write SSH acceptance record '{}': {error}",
            path.display()
        ))
    })?;
    Ok(token)
}

/// Validate a forced-command capability and recover the key it was issued
/// beside. The CLI accepts the token only after verifying the protected exec
/// boundary; caller-controlled argv has no value slot that can carry it.
pub fn verify_ssh_acceptance(
    global_root: &Path,
    machine_id: &str,
    token: &str,
) -> Result<ObservedKeys, OrbitError> {
    let path = acceptance_record_path(global_root, machine_id);
    let contents = std::fs::read_to_string(&path).map_err(|_| unauthorized_acceptance())?;
    let record =
        toml::from_str::<SshAcceptanceRecord>(&contents).map_err(|_| unauthorized_acceptance())?;
    let presented = BASE64_NO_PAD.encode(Sha256::digest(token.as_bytes()));
    if record.schema_version != 1
        || record.machine_id != machine_id
        || !digests_match(&presented, &record.token_sha256)
    {
        return Err(unauthorized_acceptance());
    }
    Ok(ObservedKeys {
        fingerprints: vec![record.key_fingerprint],
        observation: KeyObservation::DestinationCapability,
    })
}

/// Compare two token digests without leaking how far they agreed.
///
/// A digest is not itself the secret, so this is defense in depth rather than
/// the load-bearing control; it costs one fold over 43 bytes and removes the
/// one timing signal an attacker holding a candidate token could measure.
fn digests_match(presented: &str, stored: &str) -> bool {
    let (presented, stored) = (presented.as_bytes(), stored.as_bytes());
    if presented.len() != stored.len() {
        return false;
    }
    presented
        .iter()
        .zip(stored)
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn unauthorized_acceptance() -> OrbitError {
    OrbitError::UnauthorizedCaller(
        "SSH MCP acceptance was not issued by this destination; regenerate the authorized_keys \
         line with `orbit mcp callers authorize --launcher <path>`"
            .to_string(),
    )
}

fn acceptance_record_path(global_root: &Path, machine_id: &str) -> PathBuf {
    global_root
        .join(SSH_ACCEPTANCE_DIR)
        .join(format!("{machine_id}.toml"))
}

/// Parse one SSH public key, as written in a `*.pub` file.
///
/// Leading `authorized_keys` options are rejected rather than skipped: an
/// operator who passes an already-restricted line is composing a new grant
/// from an old one, and silently dropping the old options would produce a line
/// that grants more than the one it came from.
pub fn parse_public_key(contents: &str) -> Result<SshPublicKey, OrbitError> {
    let line = contents
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .ok_or_else(|| OrbitError::InvalidInput("SSH public key file is empty".to_string()))?;
    let mut fields = line.splitn(3, char::is_whitespace);
    let algorithm = fields.next().unwrap_or_default().to_string();
    let blob = fields.next().unwrap_or_default().trim().to_string();
    let comment = fields
        .next()
        .map(str::trim)
        .filter(|comment| !comment.is_empty())
        .map(ToOwned::to_owned);
    if !algorithm.starts_with("ssh-")
        && !algorithm.starts_with("ecdsa-")
        && !algorithm.starts_with("sk-")
    {
        return Err(OrbitError::InvalidInput(format!(
            "'{algorithm}' is not an SSH public key algorithm; pass the public half of the \
             caller's key, such as `~/.ssh/id_ed25519.pub`, not a private key or an \
             authorized_keys line with options"
        )));
    }
    if blob.is_empty() {
        return Err(OrbitError::InvalidInput(
            "SSH public key has no key material".to_string(),
        ));
    }
    let key = SshPublicKey {
        algorithm,
        blob,
        comment,
    };
    // Fail here rather than at the first comparison: a key that cannot be
    // fingerprinted cannot be pinned, and the operator is standing right here.
    key.fingerprint()?;
    Ok(key)
}

/// Why `candidate` is not a well-formed `SHA256:` fingerprint, if it is not.
///
/// Checked when the callers file loads so a fingerprint in the wrong format —
/// an `MD5:` one from `ssh-keygen -E md5`, most plausibly — is reported as the
/// malformed file it is, instead of silently never matching and presenting as
/// a key mismatch on every session. The detail is returned as a fragment
/// rather than an error so the loader can name the row it came from.
pub fn fingerprint_defect(candidate: &str) -> Option<String> {
    let candidate = candidate.trim();
    let Some(digest) = candidate.strip_prefix(FINGERPRINT_PREFIX) else {
        return Some(format!(
            "'{candidate}' is not a SHA-256 key fingerprint; use the `{FINGERPRINT_PREFIX}…` form \
             printed by `ssh-keygen -l -f <key>.pub`"
        ));
    };
    match BASE64_NO_PAD.decode(digest.trim_end_matches('=').as_bytes()) {
        Err(error) => Some(format!("'{candidate}' is not base64: {error}")),
        Ok(decoded) if decoded.len() != 32 => Some(format!(
            "'{candidate}' decodes to {} bytes, not the 32 a SHA-256 digest has",
            decoded.len()
        )),
        Ok(_) => None,
    }
}

/// Where a destination learned the key that authenticated this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyObservation {
    /// Recovered from a destination-issued forced-command capability.
    DestinationCapability,
    /// Read from the file sshd wrote for `ExposeAuthInfo`.
    AuthInfoFile,
}

impl KeyObservation {
    /// How the source reads in an operator-facing message.
    pub fn label(self) -> &'static str {
        match self {
            Self::DestinationCapability => "the destination-issued authorized_keys capability",
            Self::AuthInfoFile => "sshd's SSH_USER_AUTH",
        }
    }
}

/// The keys sshd accepted for this session, as this process can see them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedKeys {
    /// Every public key that authenticated, in `SHA256:` form. More than one
    /// is ordinary: a destination requiring two factors records each.
    pub fingerprints: Vec<String>,
    pub observation: KeyObservation,
}

impl ObservedKeys {
    /// Whether `pinned` is among the keys that authenticated.
    ///
    /// Padding and case in the base64 tail are normalized away so a
    /// fingerprint copied from `ssh-keygen`, from `sshd`'s log, or from this
    /// command's own output all compare equal.
    pub fn matches(&self, pinned: &str) -> bool {
        let pinned = normalize_fingerprint(pinned);
        self.fingerprints
            .iter()
            .any(|observed| normalize_fingerprint(observed) == pinned)
    }

    /// The observed keys, rendered for a refusal an operator has to act on.
    pub fn label(&self) -> String {
        if self.fingerprints.is_empty() {
            return "none".to_string();
        }
        self.fingerprints.join(", ")
    }
}

/// The key that authenticated this session, or `None` when the destination
/// cannot see it.
///
/// `None` is the ordinary Tier 1 case on a stock `sshd_config`:
/// `ExposeAuthInfo` is off by default. Tier 2 does not rely on this environment
/// path; its destination capability recovers the fingerprint recorded when the
/// forced command was generated.
pub fn observe_authenticating_keys() -> Option<ObservedKeys> {
    let path = PathBuf::from(std::env::var_os(SSH_USER_AUTH_ENV)?);
    read_auth_info(&path)
}

/// The public keys sshd's `ExposeAuthInfo` file names, as fingerprints.
///
/// Every line is `<method> <detail…>`; only `publickey` lines carry a key, and
/// only their `<algorithm> <blob>` tail is of interest. More than one is
/// ordinary — a destination requiring two factors records each — and none is
/// the honest answer for a session that authenticated without a key.
pub fn auth_info_fingerprints(contents: &str) -> Vec<String> {
    contents
        .lines()
        .filter_map(|line| line.trim().strip_prefix("publickey "))
        .filter_map(|key| {
            let blob = key.split_whitespace().nth(1)?;
            BASE64.decode(blob.as_bytes()).ok()
        })
        .map(|blob| fingerprint_of_blob(&blob))
        .collect()
}

/// Read sshd's `ExposeAuthInfo` file, or report that it cannot be read.
///
/// An unreadable file reads as "not observable" rather than as an error: sshd
/// owns that file's lifetime, and refusing a session because a temporary file
/// was already cleaned up would refuse sessions no policy meant to refuse.
///
/// A file that *was* read is an observation even when it names no key. A
/// session that authenticated by password or keyboard-interactive is one this
/// destination positively saw authenticate without a key, and reporting that
/// as "not observable" would serve a pinned row's grant to anyone holding the
/// account password.
pub(super) fn read_auth_info(path: &Path) -> Option<ObservedKeys> {
    let contents = std::fs::read_to_string(path)
        .inspect_err(|error| {
            tracing::debug!(
                target: "orbit.mcp.callers",
                path = %path.display(),
                %error,
                "SSH_USER_AUTH is set but unreadable; the authenticating key cannot be observed"
            );
        })
        .ok()?;

    Some(ObservedKeys {
        fingerprints: auth_info_fingerprints(&contents),
        observation: KeyObservation::AuthInfoFile,
    })
}

fn fingerprint_of_blob(blob: &[u8]) -> String {
    format!(
        "{FINGERPRINT_PREFIX}{}",
        BASE64_NO_PAD.encode(Sha256::digest(blob))
    )
}

fn normalize_fingerprint(fingerprint: &str) -> String {
    fingerprint
        .trim()
        .strip_prefix(FINGERPRINT_PREFIX)
        .unwrap_or(fingerprint.trim())
        .trim_end_matches('=')
        .to_string()
}
