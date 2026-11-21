//! Where the GitHub run-log cache lives under the Orbit data root, and the
//! removal of the unredacted cache earlier releases kept [ORB-26098].
//!
//! A crate-root leaf: the cache itself (the GitHub tools' `log_cache`), the
//! plugin sandbox's read mask and the crate root all read these names from
//! here, so the plugin backend does not reach into the GitHub tools.

use std::fs;
use std::path::{Path, PathBuf};

/// The run-log cache directory, relative to the Orbit data root. Broker-only:
/// every agent sandbox masks it, so a confined worker cannot read cached logs
/// except through the tool's response.
pub const GITHUB_RUN_LOG_CACHE_DIR: &str = "state/github-run-logs";

/// Where earlier releases kept unredacted entries, relative to the Orbit data
/// root. Never read; removed by [`remove_legacy_github_run_log_cache`].
const LEGACY_RAW_CACHE_DIR: &str = "cache/github-run-logs";

/// Remove the unredacted run-log cache an earlier release kept under `root`
/// (`cache/github-run-logs/`). It removes regular files directly in a plain
/// cache directory, then tries to remove that directory if it is empty. It
/// also removes the sibling `.lock` file when it is regular. Links and
/// nonfiles are skipped.
///
/// Best effort: a failed `remove_file` emits a debug diagnostic only when
/// metadata first identified that path as a regular file. Failures to inspect
/// or read the directory, iterate its entries, or remove the directory are
/// silent. There is no retry within one call; cleanup runs again the next time
/// the cache is opened with `LogCache::in_root` or an agent sandbox is prepared
/// with `prepare_agent_mask`.
pub fn remove_legacy_github_run_log_cache(root: &Path) {
    let directory = root.join(LEGACY_RAW_CACHE_DIR);
    let mut lock = directory.as_os_str().to_owned();
    lock.push(".lock");
    let is_plain_directory =
        fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.is_dir());
    let entries = is_plain_directory
        .then(|| fs::read_dir(&directory).ok())
        .flatten()
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path());
    for path in entries.chain(std::iter::once(PathBuf::from(lock))) {
        if !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
            continue;
        }
        if let Err(error) = fs::remove_file(&path) {
            tracing::debug!(%error, "legacy github run log cache file not removed");
        }
    }
    if is_plain_directory {
        let _ = fs::remove_dir(&directory);
    }
}
