//! The host's headless browser kit: its layout, its state, and the variables
//! a worker receives when it is verified.
//!
//! The kit lives at `~/.local/chromium-deps` (`kit/` holds the npm project
//! with the pinned `playwright` package, `browsers/` is
//! `PLAYWRIGHT_BROWSERS_PATH`, `env.sh` is what a check sources). `orbit
//! browser-kit` installs and verifies it; the CLI runner reads it here with
//! filesystem checks only, so inspecting it on every dispatch stays cheap.
//!
//! "Verified" means `orbit browser-kit verify` passed its smoke test against
//! the Playwright version and browser revision installed now. A kit that was
//! changed after that pass, or never passed it, is not verified, and workers
//! receive none of its variables.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The exact Playwright release this Orbit release installs, so every host on
/// the same Orbit version runs the same browser build.
pub const PLAYWRIGHT_VERSION: &str = "1.64.0";

/// The kit's location below the home directory of the user who runs workers.
pub const KIT_HOME_RELATIVE: &str = ".local/chromium-deps";

/// Where the kit's browsers live; Playwright reads it.
pub const BROWSERS_PATH_ENV: &str = "PLAYWRIGHT_BROWSERS_PATH";
/// The kit's Playwright ES module, which a check imports `chromium` from.
pub const MODULE_ENV: &str = "PLAYWRIGHT_MODULE";
/// The kit's `env.sh`, which a check sources before it launches Chromium.
/// Only that shell gets the kit's Linux libraries and font configuration.
pub const ENV_SCRIPT_ENV: &str = "ORBIT_BROWSER_KIT_ENV";

/// Every variable the CLI runner injects for a verified kit.
pub const WORKER_ENV_NAMES: [&str; 3] = [BROWSERS_PATH_ENV, MODULE_ENV, ENV_SCRIPT_ENV];

/// The record `orbit browser-kit verify` writes after its smoke test passes.
pub const VERIFICATION_FILE: &str = "orbit-kit.json";

/// First line of an `env.sh` Orbit wrote and may rewrite. A file without it
/// is the operator's and is left alone.
pub const MANAGED_ENV_SCRIPT_HEADER: &str = "# Managed by `orbit browser-kit`; edits are replaced.";

const HEADLESS_SHELL_DIR_PREFIX: &str = "chromium_headless_shell-";
const FFMPEG_DIR_PREFIX: &str = "ffmpeg-";
const HEADLESS_SHELL_EXECUTABLE: &str = "chrome-headless-shell";
const INSTALLATION_COMPLETE: &str = "INSTALLATION_COMPLETE";

/// The kit's paths, rooted at one directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KitLayout {
    root: PathBuf,
}

impl KitLayout {
    /// The kit of the user whose home is `home`.
    pub fn for_home(home: &Path) -> Self {
        Self {
            root: home.join(KIT_HOME_RELATIVE),
        }
    }

    /// The kit of the current user.
    pub fn current_user() -> Result<Self, orbit_common::OrbitError> {
        orbit_common::fs::path::home_dir().map(|home| Self::for_home(&home))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The npm project holding the pinned `playwright` package.
    pub fn npm_project(&self) -> PathBuf {
        self.root.join("kit")
    }

    pub fn browsers(&self) -> PathBuf {
        self.root.join("browsers")
    }

    pub fn env_script(&self) -> PathBuf {
        self.root.join("env.sh")
    }

    pub fn verification_file(&self) -> PathBuf {
        self.root.join(VERIFICATION_FILE)
    }

    /// Linux without root: the unpacked libraries and fonts.
    pub fn unpacked_root(&self) -> PathBuf {
        self.root.join("root")
    }

    /// Linux without root: the downloaded packages.
    pub fn debs(&self) -> PathBuf {
        self.root.join("debs")
    }

    pub fn fonts_conf(&self) -> PathBuf {
        self.root.join("fonts.conf")
    }

    pub fn fontconfig_cache(&self) -> PathBuf {
        self.root.join("fontconfig-cache")
    }

    fn playwright_package(&self) -> PathBuf {
        self.npm_project().join("node_modules/playwright")
    }

    pub fn module(&self) -> PathBuf {
        self.playwright_package().join("index.mjs")
    }

    /// Playwright's CLI script, run with `node` so `sudo` needs no PATH.
    pub fn cli_script(&self) -> PathBuf {
        self.playwright_package().join("cli.js")
    }

    /// Playwright's browser manifest, hoisted by npm or nested below the
    /// `playwright` package.
    fn browsers_manifest(&self) -> Option<PathBuf> {
        [
            self.npm_project()
                .join("node_modules/playwright-core/browsers.json"),
            self.playwright_package()
                .join("node_modules/playwright-core/browsers.json"),
        ]
        .into_iter()
        .find(|path| path.is_file())
    }

    /// Library directories unpacked under `root/` (`usr/lib/<triplet>`,
    /// `lib/<triplet>`), in that order.
    pub fn unpacked_library_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        for parent in ["usr/lib", "lib"] {
            let Ok(entries) = std::fs::read_dir(self.unpacked_root().join(parent)) else {
                continue;
            };
            let mut found: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.is_dir()
                        && path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.ends_with("-linux-gnu"))
                })
                .collect();
            found.sort();
            dirs.extend(found);
        }
        dirs
    }
}

/// What `orbit browser-kit verify` recorded when its smoke test passed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KitVerification {
    pub schema_version: u32,
    pub playwright_version: String,
    pub chromium_revision: String,
    /// The version Chromium reported, for example `156.0.8078.4`.
    pub browser_version: String,
    /// RFC 3339 time of the passing run.
    pub verified_at: String,
}

/// The kit as the filesystem shows it now.
#[derive(Debug, Clone, Serialize)]
pub struct KitInspection {
    pub root: PathBuf,
    /// The kit directory exists.
    pub present: bool,
    /// Version of the installed `playwright` package.
    pub playwright_version: Option<String>,
    /// This Orbit release's pin.
    pub pinned_version: &'static str,
    pub module_present: bool,
    /// The headless-shell revision the installed package launches.
    pub chromium_revision: Option<String>,
    /// That revision is fully downloaded under `browsers/`.
    pub browser_installed: bool,
    /// The headless shell's executable, when the revision is installed.
    pub executable: Option<PathBuf>,
    /// `browsers/` holds revisions other than the expected ones.
    pub stale_revisions: Vec<String>,
    pub env_script_present: bool,
    /// `env.sh` is Orbit's, so `install` may rewrite it.
    pub env_script_managed: bool,
    /// The last passing smoke test, when one is recorded.
    pub verification: Option<KitVerification>,
}

impl KitInspection {
    pub fn inspect(layout: &KitLayout) -> Self {
        let present = layout.root().is_dir();
        let playwright_version = read_json(&layout.playwright_package().join("package.json"))
            .and_then(|package| package.get("version")?.as_str().map(str::to_string));
        let revisions = layout
            .browsers_manifest()
            .and_then(|manifest| read_json(&manifest))
            .map(|manifest| ManifestRevisions::from_manifest(&manifest))
            .unwrap_or_default();
        let shell_dir = revisions.chromium.as_ref().map(|revision| {
            layout
                .browsers()
                .join(format!("{HEADLESS_SHELL_DIR_PREFIX}{revision}"))
        });
        let browser_installed = shell_dir
            .as_ref()
            .is_some_and(|dir| dir.join(INSTALLATION_COMPLETE).is_file());
        let executable = shell_dir
            .filter(|_| browser_installed)
            .and_then(|dir| find_executable(&dir, HEADLESS_SHELL_EXECUTABLE, 3));
        let stale_revisions = stale_revisions(&layout.browsers(), &revisions);
        let env_script = std::fs::read_to_string(layout.env_script()).ok();
        let verification = std::fs::read(layout.verification_file())
            .ok()
            .and_then(|bytes| serde_json::from_slice::<KitVerification>(&bytes).ok());
        Self {
            root: layout.root().to_path_buf(),
            present,
            playwright_version,
            pinned_version: PLAYWRIGHT_VERSION,
            module_present: layout.module().is_file(),
            chromium_revision: revisions.chromium,
            browser_installed,
            executable,
            stale_revisions,
            env_script_present: env_script.is_some(),
            env_script_managed: env_script
                .as_deref()
                .is_some_and(|text| text.starts_with(MANAGED_ENV_SCRIPT_HEADER)),
            verification,
        }
    }

    /// The installed package is the version this Orbit release pins.
    pub fn matches_pin(&self) -> bool {
        self.playwright_version.as_deref() == Some(PLAYWRIGHT_VERSION)
    }

    /// Module, browser and `env.sh` are in place, and the recorded smoke test
    /// ran against this exact package version and browser revision.
    pub fn is_verified(&self) -> bool {
        let Some(verification) = &self.verification else {
            return false;
        };
        self.module_present
            && self.browser_installed
            && self.env_script_present
            && self.playwright_version.as_deref() == Some(verification.playwright_version.as_str())
            && self.chromium_revision.as_deref() == Some(verification.chromium_revision.as_str())
    }

    /// The variables a worker receives: all three for a verified kit, none
    /// otherwise.
    pub fn worker_env(&self, layout: &KitLayout) -> Vec<(String, String)> {
        if !self.is_verified() {
            return Vec::new();
        }
        vec![
            (
                BROWSERS_PATH_ENV.to_string(),
                layout.browsers().display().to_string(),
            ),
            (
                MODULE_ENV.to_string(),
                layout.module().display().to_string(),
            ),
            (
                ENV_SCRIPT_ENV.to_string(),
                layout.env_script().display().to_string(),
            ),
        ]
    }
}

/// The variables a worker whose home is `home` receives from its kit.
pub fn worker_env_for_home(home: &Path) -> Vec<(String, String)> {
    let layout = KitLayout::for_home(home);
    if !layout.root().is_dir() {
        return Vec::new();
    }
    KitInspection::inspect(&layout).worker_env(&layout)
}

#[derive(Debug, Default)]
struct ManifestRevisions {
    chromium: Option<String>,
    ffmpeg: Option<String>,
}

impl ManifestRevisions {
    fn from_manifest(manifest: &serde_json::Value) -> Self {
        let revision = |name: &str| {
            manifest
                .get("browsers")?
                .as_array()?
                .iter()
                .find(|browser| browser.get("name").and_then(|value| value.as_str()) == Some(name))?
                .get("revision")?
                .as_str()
                .map(str::to_string)
        };
        Self {
            chromium: revision("chromium-headless-shell"),
            ffmpeg: revision("ffmpeg"),
        }
    }
}

/// Headless-shell and ffmpeg revisions in `browsers/` that the installed
/// package does not use. Empty while the expected revision is unknown, so an
/// incomplete install never marks every revision stale.
fn stale_revisions(browsers: &Path, expected: &ManifestRevisions) -> Vec<String> {
    let Some(chromium) = expected.chromium.as_deref() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(browsers) else {
        return Vec::new();
    };
    let mut stale: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| {
            if let Some(revision) = name.strip_prefix(HEADLESS_SHELL_DIR_PREFIX) {
                revision != chromium
            } else if let Some(revision) = name.strip_prefix(FFMPEG_DIR_PREFIX) {
                expected
                    .ffmpeg
                    .as_deref()
                    .is_some_and(|ffmpeg| revision != ffmpeg)
            } else {
                false
            }
        })
        .collect();
    stale.sort();
    stale
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// A regular file named `name` at most `depth` directories below `dir`.
fn find_executable(dir: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    if let Some(found) = entries
        .iter()
        .find(|path| path.is_file() && path.file_name().is_some_and(|file| file == name))
    {
        return Some(found.clone());
    }
    if depth == 0 {
        return None;
    }
    entries
        .iter()
        .filter(|path| path.is_dir())
        .find_map(|path| find_executable(path, name, depth - 1))
}
