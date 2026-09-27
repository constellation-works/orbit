//! Where published release artifacts are read from.
//!
//! Two implementations sit behind one trait so the update flow never branches
//! on transport: [`HttpReleaseSource`] against GitHub Releases, and
//! [`DirectoryReleaseSource`] against a local mirror. The mirror is a real
//! operator feature for air-gapped and staged rollouts, and it is also what
//! lets the update tests exercise download, signature, and checksum handling
//! against fixtures instead of the network.

use std::fmt::Debug;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_common::security::release::{
    RELEASE_CHECKSUMS_FILENAME, RELEASE_CHECKSUMS_SIGNATURE_FILENAME,
};

/// Input ceilings apply before authentication and before any complete artifact is buffered.
pub(super) const MAX_METADATA_BYTES: u64 = 64 * 1024;
pub(super) const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
pub(super) const MAX_SIGNATURE_BYTES: u64 = 16 * 1024;
pub(super) const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;

fn asset_limit(asset: &str) -> u64 {
    match asset {
        RELEASE_CHECKSUMS_FILENAME => MAX_MANIFEST_BYTES,
        RELEASE_CHECKSUMS_SIGNATURE_FILENAME => MAX_SIGNATURE_BYTES,
        _ => MAX_ARCHIVE_BYTES,
    }
}

fn too_large(what: &str, limit: u64) -> OrbitError {
    OrbitError::Execution(format!(
        "{what} exceeds the {limit}-byte release input limit"
    ))
}

pub(super) fn read_bounded(
    reader: impl Read,
    what: &str,
    limit: u64,
) -> Result<Vec<u8>, OrbitError> {
    let mut bytes = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| OrbitError::Execution(format!("failed to read {what}: {error}")))?;
    if bytes.len() as u64 > limit {
        return Err(too_large(what, limit));
    }
    Ok(bytes)
}

/// Repository the HTTP source reads releases from unless overridden.
pub const DEFAULT_RELEASE_REPO: &str = "constellation-works/orbit";

/// Environment variable naming a local release mirror directory.
pub const RELEASE_DIR_ENV: &str = "ORBIT_UPDATE_RELEASE_DIR";

/// Environment variable naming the GitHub repository to read releases from.
/// Shares its name with the variable `install.sh` reads.
pub const RELEASE_REPO_ENV: &str = "ORBIT_INSTALL_REPO";

/// File in a mirror directory naming the newest published version.
pub const MIRROR_LATEST_FILE: &str = "latest-version.txt";

/// A source of published Orbit release artifacts.
pub trait ReleaseSource: Debug + Send + Sync {
    /// Where these artifacts come from, for diagnostics and reports.
    fn describe(&self) -> String;

    /// The newest published version, without a leading `v`.
    fn latest_version(&self) -> Result<String, OrbitError>;

    /// Fetch `asset` as published with `version`.
    fn fetch(&self, version: &str, asset: &str) -> Result<Vec<u8>, OrbitError>;
}

/// Build the release source this process should use.
///
/// A configured mirror wins over the network so an air-gapped host never
/// silently reaches out.
pub fn release_source_from_env() -> Box<dyn ReleaseSource> {
    if let Some(dir) = std::env::var_os(RELEASE_DIR_ENV).filter(|value| !value.is_empty()) {
        return Box::new(DirectoryReleaseSource::new(PathBuf::from(dir)));
    }
    let repo = std::env::var(RELEASE_REPO_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_RELEASE_REPO.to_string());
    Box::new(HttpReleaseSource::new(repo))
}

/// GitHub Releases over HTTPS.
#[derive(Debug, Clone)]
pub struct HttpReleaseSource {
    repo: String,
    timeout: Duration,
}

impl HttpReleaseSource {
    /// Read releases from `owner/name` on github.com.
    pub fn new(repo: String) -> Self {
        Self {
            repo,
            timeout: Duration::from_secs(120),
        }
    }

    fn client(&self) -> Result<reqwest::blocking::Client, OrbitError> {
        reqwest::blocking::Client::builder()
            .timeout(self.timeout)
            // GitHub's REST API rejects requests without one.
            .user_agent(concat!("orbit-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| {
                OrbitError::Execution(format!("failed to build release HTTP client: {error}"))
            })
    }

    pub(super) fn get(&self, url: &str, what: &str, limit: u64) -> Result<Vec<u8>, OrbitError> {
        let response = self
            .client()?
            .get(url)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| {
                OrbitError::Execution(format!("failed to download {what} from {url}: {error}"))
            })?;
        if response
            .content_length()
            .is_some_and(|length| length > limit)
        {
            return Err(too_large(what, limit));
        }
        read_bounded(response, what, limit)
    }
}

impl ReleaseSource for HttpReleaseSource {
    fn describe(&self) -> String {
        format!("https://github.com/{}/releases", self.repo)
    }

    fn latest_version(&self) -> Result<String, OrbitError> {
        let url = format!("https://api.github.com/repos/{}/releases/latest", self.repo);
        let body = self.get(&url, "the latest release metadata", MAX_METADATA_BYTES)?;
        let document: serde_json::Value = serde_json::from_slice(&body).map_err(|error| {
            OrbitError::Execution(format!("latest release metadata is not JSON: {error}"))
        })?;
        let tag = document
            .get("tag_name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                OrbitError::Execution(
                    "latest release metadata has no `tag_name`; cannot resolve the newest version"
                        .to_string(),
                )
            })?;
        Ok(tag.trim_start_matches('v').to_string())
    }

    fn fetch(&self, version: &str, asset: &str) -> Result<Vec<u8>, OrbitError> {
        let url = format!(
            "https://github.com/{}/releases/download/v{version}/{asset}",
            self.repo
        );
        self.get(&url, asset, asset_limit(asset))
    }
}

/// A local mirror laid out as `<root>/latest-version.txt` plus
/// `<root>/v<version>/<asset>`.
#[derive(Debug, Clone)]
pub struct DirectoryReleaseSource {
    root: PathBuf,
}

impl DirectoryReleaseSource {
    /// Read releases from the mirror rooted at `root`.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn read(path: &Path, what: &str, limit: u64) -> Result<Vec<u8>, OrbitError> {
        let file = std::fs::File::open(path).map_err(|error| {
            OrbitError::Execution(format!(
                "failed to read {what} from the release mirror at '{}': {error}",
                path.display()
            ))
        })?;
        if file
            .metadata()
            .map_err(|error| {
                OrbitError::Execution(format!(
                    "failed to inspect {what} in the release mirror at '{}': {error}",
                    path.display()
                ))
            })?
            .len()
            > limit
        {
            return Err(too_large(what, limit));
        }
        read_bounded(file, what, limit)
    }
}

impl ReleaseSource for DirectoryReleaseSource {
    fn describe(&self) -> String {
        format!("release mirror {}", self.root.display())
    }

    fn latest_version(&self) -> Result<String, OrbitError> {
        let path = self.root.join(MIRROR_LATEST_FILE);
        let body = Self::read(&path, MIRROR_LATEST_FILE, MAX_METADATA_BYTES)?;
        let version = String::from_utf8(body)
            .map_err(|error| {
                OrbitError::Execution(format!("{MIRROR_LATEST_FILE} is not UTF-8: {error}"))
            })?
            .trim()
            .trim_start_matches('v')
            .to_string();
        if version.is_empty() {
            return Err(OrbitError::Execution(format!(
                "{} is empty; the mirror publishes no latest version",
                path.display()
            )));
        }
        Ok(version)
    }

    fn fetch(&self, version: &str, asset: &str) -> Result<Vec<u8>, OrbitError> {
        // Both segments are Orbit-generated (a parsed version, a built asset
        // name), but joining unvalidated components onto a mirror root is
        // exactly the shape that escapes it, so reject separators outright.
        for segment in [version, asset] {
            if segment.contains('/') || segment.contains('\\') || segment.contains("..") {
                return Err(OrbitError::InvalidInput(format!(
                    "release mirror path segment '{segment}' must not contain a path separator"
                )));
            }
        }
        Self::read(
            &self.root.join(format!("v{version}")).join(asset),
            asset,
            asset_limit(asset),
        )
    }
}
