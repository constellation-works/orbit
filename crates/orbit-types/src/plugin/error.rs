use thiserror::Error;

use super::grant::PluginGrant;
use super::pin::PIN_FILE_SCHEMA_VERSION;
use super::version::VersionError;

/// A `--grant` list or recorded grant set that does not name an
/// unambiguous set of grants.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PluginGrantError {
    /// Every name in `names` is neither a grant nor a root continuation.
    #[error(
        "unknown grant{} {}; valid grants are {}",
        if .names.len() == 1 { "" } else { "s" },
        .names.join(", "),
        valid_grants()
    )]
    Unknown { names: Vec<String> },
    #[error("grant `{grant}` does not take roots; write `{grant}` on its own")]
    RootsNotTaken { grant: PluginGrant },
    #[error(
        "`{grant}=` needs at least one root; write `{grant}` on its own to grant every root the \
         manifest requests"
    )]
    RootMissing { grant: PluginGrant },
    #[error(
        "grant `{grant}` was given both with and without roots; write `{grant}=<root>[,<root>]` \
         to scope it, or `{grant}` on its own to grant every root the manifest requests"
    )]
    ScopedAndUnscoped { grant: PluginGrant },
    /// The recorded spelling `listed` of an accepted set fails to parse.
    #[error("grant set `{listed}` does not parse back: {reason}")]
    RecordedUnparseable {
        listed: String,
        reason: Box<PluginGrantError>,
    },
    /// The recorded spelling `listed` parses back as the different set
    /// `loaded`.
    #[error("grant set `{listed}` loads back as `{loaded}`")]
    RecordedDrift { listed: String, loaded: String },
}

fn valid_grants() -> String {
    PluginGrant::ALL
        .iter()
        .map(|grant| grant.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A pinned archive digest that is not a `sha256:` digest.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ArchiveDigestError {
    #[error("'{value}' is not a supported digest; use `sha256:<64 hex characters>`")]
    UnsupportedAlgorithm { value: String },
    #[error("'{value}' is not a `sha256:` digest of 64 hex characters")]
    Malformed { value: String },
}

/// A `.orbit/plugins.yaml` pin file that breaks the pin contract; `index`
/// is the offending entry in `plugins`.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PluginPinError {
    #[error(
        "schemaVersion: unsupported pin file schemaVersion {found}; expected {}",
        PIN_FILE_SCHEMA_VERSION
    )]
    UnsupportedSchemaVersion { found: u32 },
    #[error("plugins[{index}].name: '{name}' is not a valid plugin namespace")]
    InvalidName { index: usize, name: String },
    #[error("plugins[{index}].name: '{name}' is pinned more than once")]
    DuplicateName { index: usize, name: String },
    #[error("plugins[{index}].version: {reason}")]
    InvalidVersion { index: usize, reason: VersionError },
    #[error("plugins[{index}].digest: {reason}")]
    InvalidDigest {
        index: usize,
        reason: ArchiveDigestError,
    },
    /// A digest beside a source Orbit never fetches, so nothing checks it.
    #[error(
        "plugins[{index}].digest: only an `https://` archive source is digest-verified; remove \
         the digest or pin an archive URL"
    )]
    DigestWithoutArchive { index: usize },
    #[error(
        "plugins[{index}].digest: the archive source '{url}' must pin a `sha256:` digest; Orbit \
         does not trust a fetched archive on first use"
    )]
    ArchiveDigestMissing { index: usize, url: String },
    /// An artifact digest beside a source that does not name one commit.
    #[error(
        "plugins[{index}].artifact_digest: only a `git+<url>#<full commit id>` source is built \
         and digested; pin the source to a 40- or 64-character commit or remove the artifact \
         digest"
    )]
    ArtifactDigestWithoutCommit { index: usize },
    #[error("plugins[{index}].artifact_digest: {reason}")]
    InvalidArtifactDigest {
        index: usize,
        reason: ArchiveDigestError,
    },
}
