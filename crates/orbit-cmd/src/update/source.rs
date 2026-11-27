//! Where published release artifacts are read from.
//!
//! Two implementations sit behind one trait so the update flow never branches
//! on transport: [`HttpReleaseSource`] against GitHub Releases, and
//! [`DirectoryReleaseSource`] against a local mirror. The mirror is a real
//! operator feature for air-gapped and staged rollouts, and it is also what
//! lets the update tests exercise download, signature, and checksum handling
//! against fixtures instead of the network.

mod unpack;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Environment variable naming the GitHub repository to read releases from.
/// Shares its name with the variable `install.sh` reads.
pub use orbit_common::env_vars::ORBIT_INSTALL_REPO as RELEASE_REPO_ENV;
/// Environment variable naming a local release mirror directory.
pub use orbit_common::env_vars::ORBIT_UPDATE_RELEASE_DIR as RELEASE_DIR_ENV;
use orbit_common::observability::logging::format_error_chain;
use orbit_common::{Env, OrbitError};
pub(super) use orbit_core::app::update::source::MAX_METADATA_BYTES;
pub use orbit_core::app::update::source::ReleaseSource;
use orbit_core::app::update::source::{
    MAX_RELEASE_METADATA_BYTES, asset_limit, read_bounded, too_large,
};

/// Repository the HTTP source reads releases from unless overridden.
pub const DEFAULT_RELEASE_REPO: &str = "constellation-works/orbit";

/// File in a mirror directory naming the newest published version.
pub const MIRROR_LATEST_FILE: &str = "latest-version.txt";

/// Hosts a release request may be addressed to. Asset downloads redirect from
/// `github.com` to GitHub's object storage; the client's redirect policy keeps
/// those hops on HTTPS too.
const RELEASE_HOSTS: [&str; 2] = ["api.github.com", "github.com"];

/// Most redirects one release request follows, matching reqwest's default.
const MAX_RELEASE_REDIRECTS: usize = 10;

/// Parse a release request URL, requiring HTTPS to a GitHub release host.
///
/// Release metadata and artifacts never travel in cleartext. The configured
/// repository name only ever lands in the path, so this also pins the host.
/// Code scanning treats the returned URL as a cleartext-transmission barrier
/// (`.github/codeql/extensions/orbit-rust-release-transport`).
pub(super) fn validated_release_url(url: &str) -> Result<reqwest::Url, OrbitError> {
    let parsed = reqwest::Url::parse(url).map_err(|error| {
        OrbitError::InvalidInput(format!("release URL '{url}' is not a URL: {error}"))
    })?;
    let host_allowed = parsed
        .host_str()
        .is_some_and(|host| RELEASE_HOSTS.contains(&host));
    if parsed.scheme() != "https" || !host_allowed {
        return Err(OrbitError::InvalidInput(format!(
            "release URL '{url}' must be https:// on {}",
            RELEASE_HOSTS.join(" or ")
        )));
    }
    Ok(parsed)
}

/// Follow redirects only while they stay on HTTPS.
fn https_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.url().scheme() != "https" {
            let refused = format!("refusing a non-HTTPS release redirect to {}", attempt.url());
            attempt.error(refused)
        } else if attempt.previous().len() >= MAX_RELEASE_REDIRECTS {
            attempt.error("too many release redirects")
        } else {
            attempt.follow()
        }
    })
}

/// Build the release source this process should use.
///
/// A configured mirror wins over the network so an air-gapped host never
/// silently reaches out.
pub fn release_source_from_env(env: &Env) -> Box<dyn ReleaseSource> {
    if let Some(dir) = env
        .orbit_update_release_dir()
        .filter(|value| !value.is_empty())
    {
        return Box::new(DirectoryReleaseSource::new(PathBuf::from(dir)));
    }
    let repo = env
        .orbit_install_repo()
        .and_then(OsStr::to_str)
        .filter(|value| !value.is_empty())
        .map_or_else(|| DEFAULT_RELEASE_REPO.to_string(), str::to_string);
    #[cfg(all(feature = "test-release-api-base", debug_assertions))]
    let test_api_base = env
        .orbit_test_release_api_base()
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned());
    let source = HttpReleaseSource::new(repo);
    #[cfg(all(feature = "test-release-api-base", debug_assertions))]
    let source = source.with_test_api_base(test_api_base);
    Box::new(source)
}

/// GitHub Releases over HTTPS.
#[derive(Debug, Clone)]
pub struct HttpReleaseSource {
    repo: String,
    timeout: Duration,
    #[cfg(all(feature = "test-release-api-base", debug_assertions))]
    test_api_base: Option<String>,
}

impl HttpReleaseSource {
    /// Read releases from `owner/name` on github.com.
    pub fn new(repo: String) -> Self {
        Self {
            repo,
            timeout: Duration::from_secs(120),
            #[cfg(all(feature = "test-release-api-base", debug_assertions))]
            test_api_base: None,
        }
    }

    #[cfg(all(feature = "test-release-api-base", debug_assertions))]
    fn with_test_api_base(mut self, test_api_base: Option<String>) -> Self {
        self.test_api_base = test_api_base;
        self
    }

    fn client(&self) -> Result<reqwest::blocking::Client, OrbitError> {
        reqwest::blocking::Client::builder()
            .timeout(self.timeout)
            // GitHub's REST API rejects requests without one.
            .user_agent(concat!("orbit-cli/", env!("CARGO_PKG_VERSION")))
            .redirect(https_redirect_policy())
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
                OrbitError::Execution(format!(
                    "failed to download {what} from {url}: {}",
                    format_error_chain(&error)
                ))
            })?;
        if response
            .content_length()
            .is_some_and(|length| length > limit)
        {
            return Err(too_large(what, limit));
        }
        read_bounded(response, what, limit)
    }

    fn latest_release_url(&self) -> Result<reqwest::Url, OrbitError> {
        let path = format!("repos/{}/releases/latest", self.repo);

        // This seam exists only in the CLI test build, where it serves a local
        // loopback fixture. Keeping it behind both a dev-only feature and
        // debug_assertions means neither release builds nor user configuration
        // can redirect release metadata requests.
        #[cfg(all(feature = "test-release-api-base", debug_assertions))]
        if let Some(base) = &self.test_api_base {
            return test_release_api_url(base, &path);
        }

        validated_release_url(&format!("https://api.github.com/{path}"))
    }
}

/// Validate the loopback-only endpoint used by the CLI process-test feature.
#[cfg(all(feature = "test-release-api-base", debug_assertions))]
fn test_release_api_url(base: &str, path: &str) -> Result<reqwest::Url, OrbitError> {
    let parsed = reqwest::Url::parse(base).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "test release API base '{base}' is not a URL: {error}"
        ))
    })?;
    let loopback_ip = parsed
        .host_str()
        .and_then(|host| host.parse::<std::net::IpAddr>().ok())
        .is_some_and(|address| address.is_loopback());
    if parsed.scheme() != "http"
        || !loopback_ip
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(OrbitError::InvalidInput(
            "test release API base must be an HTTP loopback origin".to_string(),
        ));
    }
    parsed.join(path).map_err(|error| {
        OrbitError::InvalidInput(format!("test release API path is invalid: {error}"))
    })
}

impl ReleaseSource for HttpReleaseSource {
    fn unpack(&self, archive: &[u8], asset: &str) -> Result<Vec<u8>, OrbitError> {
        unpack::extract_release_executable(archive, asset)
    }

    fn describe(&self) -> String {
        format!("https://github.com/{}/releases", self.repo)
    }

    fn latest_version(&self) -> Result<String, OrbitError> {
        let url = self.latest_release_url()?;
        let body = self.get(
            url.as_str(),
            "the latest release metadata",
            MAX_RELEASE_METADATA_BYTES,
        )?;
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
        let url = validated_release_url(&format!(
            "https://github.com/{}/releases/download/v{version}/{asset}",
            self.repo
        ))?;
        self.get(url.as_str(), asset, asset_limit(asset))
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
    fn unpack(&self, archive: &[u8], asset: &str) -> Result<Vec<u8>, OrbitError> {
        unpack::extract_release_executable(archive, asset)
    }

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
