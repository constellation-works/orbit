//! Finding and authenticating the newest desktop release.
//!
//! The app lists the repository's recent GitHub releases and picks the newest
//! stable one, above the running version, that carries the desktop manifest,
//! its signature, the version file and this platform's bundle. Releases
//! without desktop assets (made while `ORBIT_DESKTOP_RELEASE` is off, or
//! whose desktop jobs failed or haven't finished) are skipped, so they never
//! hide an older release that has them.
//!
//! The manifest is signed, and it lists the version file's digest. Asset names
//! carry no version, so the version file is what ties a signed asset set to a
//! release: without it, an older signed set uploaded under a newer tag would
//! verify.

use std::fmt::{Display, Formatter};
use std::io::{Read, Write};
use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::trust::{self, TrustedReleaseKey};

/// The repository that publishes Orbit releases.
const REPOSITORY: &str = "constellation-works/orbit";
/// Releases read per check, newest first. A run of CLI-only releases longer
/// than this hides the last desktop release until the next one ships.
const RELEASES_PER_CHECK: usize = 20;

pub(crate) const MANIFEST: &str = "orbit-desktop-checksums.txt";
pub(crate) const SIGNATURE: &str = "orbit-desktop-checksums.txt.sig";
pub(crate) const VERSION_FILE: &str = "orbit-desktop-version.txt";

/// Size ceilings for each download.
const MAX_LISTING_BYTES: u64 = 8 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_SIGNATURE_BYTES: u64 = 4 * 1024;
const MAX_VERSION_BYTES: u64 = 256;
const MAX_BUNDLE_BYTES: u64 = 1024 * 1024 * 1024;

/// Upper bound on a metadata request, connection included.
const METADATA_TIMEOUT: Duration = Duration::from_secs(30);
/// Upper bounds on a bundle download: connecting or waiting for the response
/// headers, and the whole transfer.
const BUNDLE_START_TIMEOUT: Duration = Duration::from_secs(60);
const BUNDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// A stable release version, `MAJOR.MINOR.PATCH`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Version(u64, u64, u64);

impl Version {
    /// Parse `1.2.3` or `v1.2.3`. Prerelease and build suffixes are rejected:
    /// the app updates only between stable releases.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text.split('.');
        let mut next = || -> Option<u64> {
            let part = parts.next()?;
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        };
        let version = Self(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }

    /// The running app's version.
    pub(crate) fn current() -> Option<Self> {
        Self::parse(env!("CARGO_PKG_VERSION"))
    }
}

impl Display for Version {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// Pick the newest stable release above `current` that carries every desktop
/// asset this platform needs. `releases` is GitHub's release-list JSON.
pub(crate) fn newest_update(releases: &Value, current: Version, bundle: &str) -> Option<Version> {
    releases
        .as_array()?
        .iter()
        .filter(|release| {
            release["draft"].as_bool() == Some(false)
                && release["prerelease"].as_bool() == Some(false)
        })
        .filter_map(|release| {
            let version = Version::parse(release["tag_name"].as_str()?)?;
            let names: Vec<&str> = release["assets"]
                .as_array()?
                .iter()
                .filter_map(|asset| asset["name"].as_str())
                .collect();
            [MANIFEST, SIGNATURE, VERSION_FILE, bundle]
                .iter()
                .all(|needed| names.contains(needed))
                .then_some(version)
        })
        .filter(|version| *version > current)
        .max()
}

/// Why a check found no update to offer.
#[derive(Debug)]
pub(crate) enum CheckError {
    /// GitHub couldn't be read, or the release is incomplete. Not alarming.
    Unavailable(String),
    /// The release's desktop assets failed authentication.
    Rejected(String),
}

/// An update whose manifest has been authenticated, ready to download.
#[derive(Debug, Clone)]
pub(crate) struct Verified {
    pub(crate) version: Version,
    /// The platform bundle's asset name and its signed SHA-256.
    pub(crate) bundle: String,
    sha256: String,
}

/// HTTPS access to GitHub releases.
pub(crate) struct Releases {
    metadata: ureq::Agent,
    bundles: ureq::Agent,
}

impl Releases {
    pub(crate) fn new() -> Self {
        let user_agent = format!("orbit-desktop/{}", env!("CARGO_PKG_VERSION"));
        let metadata = ureq::Agent::config_builder()
            .https_only(true)
            .timeout_global(Some(METADATA_TIMEOUT))
            .user_agent(user_agent.as_str())
            .build()
            .into();
        let bundles = ureq::Agent::config_builder()
            .https_only(true)
            .timeout_connect(Some(BUNDLE_START_TIMEOUT))
            .timeout_recv_response(Some(BUNDLE_START_TIMEOUT))
            .timeout_global(Some(BUNDLE_TIMEOUT))
            .user_agent(user_agent.as_str())
            .build()
            .into();
        Self { metadata, bundles }
    }

    /// Find an update for `bundle` and authenticate its manifest and version.
    pub(crate) fn check(
        &self,
        current: Version,
        bundle: &str,
        keys: &[TrustedReleaseKey],
        today: chrono::NaiveDate,
    ) -> Result<Option<Verified>, CheckError> {
        let url = format!(
            "https://api.github.com/repos/{REPOSITORY}/releases?per_page={RELEASES_PER_CHECK}"
        );
        let listing = self
            .metadata
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .call()
            .map_err(|error| CheckError::Unavailable(format!("could not list releases: {error}")))?
            .body_mut()
            .with_config()
            .limit(MAX_LISTING_BYTES)
            .read_to_vec()
            .map_err(|error| {
                CheckError::Unavailable(format!("could not list releases: {error}"))
            })?;
        let releases: Value = serde_json::from_slice(&listing).map_err(|error| {
            CheckError::Unavailable(format!("the release list is not JSON: {error}"))
        })?;
        let Some(version) = newest_update(&releases, current, bundle) else {
            return Ok(None);
        };
        let fetch = |asset: &str, limit: u64| {
            self.fetch_small(version, asset, limit)
                .map_err(CheckError::Unavailable)
        };
        let manifest = fetch(MANIFEST, MAX_MANIFEST_BYTES)?;
        let signature = fetch(SIGNATURE, MAX_SIGNATURE_BYTES)?;
        trust::verify_manifest(&manifest, &signature, keys, today).map_err(|error| {
            CheckError::Rejected(format!("Orbit {version}'s desktop checksums: {error}"))
        })?;
        let manifest = String::from_utf8(manifest).map_err(|_| {
            CheckError::Rejected(format!("Orbit {version}'s desktop checksums are not UTF-8"))
        })?;
        let digest_of = |asset: &str| {
            trust::checksum_for(&manifest, asset).ok_or_else(|| {
                CheckError::Unavailable(format!(
                    "Orbit {version}'s desktop checksums don't list {asset}"
                ))
            })
        };
        let version_digest = digest_of(VERSION_FILE)?;
        let sha256 = digest_of(bundle)?;
        let version_file = fetch(VERSION_FILE, MAX_VERSION_BYTES)?;
        if trust::sha256_hex(&version_file) != version_digest {
            return Err(CheckError::Rejected(format!(
                "Orbit {version}'s {VERSION_FILE} doesn't match its signed checksum"
            )));
        }
        let signed_version = std::str::from_utf8(&version_file)
            .ok()
            .and_then(Version::parse);
        if signed_version != Some(version) {
            return Err(CheckError::Rejected(format!(
                "release v{version} carries the desktop assets of another version"
            )));
        }
        Ok(Some(Verified {
            version,
            bundle: bundle.to_string(),
            sha256,
        }))
    }

    /// Stream `update`'s bundle into `file`, failing unless it hashes to the
    /// signed digest. The caller discards the file on failure.
    pub(crate) fn download(
        &self,
        update: &Verified,
        file: &mut std::fs::File,
    ) -> Result<(), String> {
        let url = asset_url(update.version, &update.bundle);
        let mut response = self
            .bundles
            .get(&url)
            .call()
            .map_err(|error| format!("could not download {}: {error}", update.bundle))?;
        let mut reader = response
            .body_mut()
            .with_config()
            .limit(MAX_BUNDLE_BYTES)
            .reader();
        let mut hasher = Sha256::new();
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|error| format!("could not download {}: {error}", update.bundle))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])
                .map_err(|error| format!("could not save {}: {error}", update.bundle))?;
        }
        // The bundle replaces the running app next, so it must be on disk.
        #[expect(
            clippy::disallowed_methods,
            reason = "orbit_common::fs::io isn't available in this workspace"
        )]
        file.sync_all()
            .map_err(|error| format!("could not save {}: {error}", update.bundle))?;
        let actual = trust::hex(&hasher.finalize());
        if actual != update.sha256 {
            return Err(format!(
                "{} doesn't match its signed checksum (expected {}, got {actual})",
                update.bundle, update.sha256
            ));
        }
        Ok(())
    }

    fn fetch_small(&self, version: Version, asset: &str, limit: u64) -> Result<Vec<u8>, String> {
        self.metadata
            .get(&asset_url(version, asset))
            .call()
            .map_err(|error| format!("could not download {asset}: {error}"))?
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .map_err(|error| format!("could not download {asset}: {error}"))
    }
}

/// Download URL of a release asset. The tag comes from a parsed version, so
/// the listing's own URLs are never followed.
fn asset_url(version: Version, asset: &str) -> String {
    format!("https://github.com/{REPOSITORY}/releases/download/v{version}/{asset}")
}
