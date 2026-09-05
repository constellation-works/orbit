//! Which installer owns the running executable, and which release asset fits
//! this machine.
//!
//! `orbit update` replaces a binary in place. That is only ever correct when
//! Orbit's own installer put it there: a Homebrew formula, an npm package, or
//! `cargo install` each keep their own manifest of what they installed, and
//! overwriting their file silently desynchronizes it from the manager that
//! will next upgrade or uninstall it. So detection is not a nicety here — it
//! is the guard that decides between updating and handing the operator the
//! one command that actually works for them.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;

/// Environment variable `install.sh` reads for the managed install directory.
pub const INSTALL_DIR_ENV: &str = "ORBIT_INSTALL_DIR";

/// The installer that owns the current executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallChannel {
    /// Installed by `install.sh` into the managed bin directory. The only
    /// channel `orbit update` may replace in place.
    Managed {
        /// Directory holding the managed `orbit` executable.
        install_dir: PathBuf,
    },
    /// The npm package `@orbit-tools/cli` owns this binary.
    Npm,
    /// A Homebrew formula owns this binary.
    Homebrew,
    /// `cargo install` or `make install` owns this binary.
    Cargo,
    /// A build tree inside a checkout's `target/` directory.
    LocalBuild,
    /// Somewhere Orbit does not recognize.
    Unknown,
}

impl InstallChannel {
    /// Stable identifier for JSON output and audit rows.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Managed { .. } => "managed",
            Self::Npm => "npm",
            Self::Homebrew => "homebrew",
            Self::Cargo => "cargo",
            Self::LocalBuild => "local-build",
            Self::Unknown => "unknown",
        }
    }

    /// Classify `executable` given the managed install directory this machine
    /// is configured for.
    ///
    /// The managed directory is checked first: setting `ORBIT_INSTALL_DIR` is
    /// an explicit statement about who owns that path, and it outranks any
    /// inference drawn from the path's shape.
    pub fn detect(executable: &Path, managed_install_dir: Option<&Path>) -> Self {
        let parent = executable.parent();
        if let (Some(parent), Some(managed)) = (parent, managed_install_dir)
            && paths_equal(parent, managed)
        {
            return Self::Managed {
                install_dir: managed.to_path_buf(),
            };
        }
        if has_component(executable, "node_modules") {
            return Self::Npm;
        }
        if has_component(executable, "Cellar") || has_component(executable, "homebrew") {
            return Self::Homebrew;
        }
        if is_cargo_build_output(executable) {
            return Self::LocalBuild;
        }
        if parent.is_some_and(|parent| parent.ends_with(".cargo/bin")) {
            return Self::Cargo;
        }
        Self::Unknown
    }

    /// Refuse an in-place replacement, naming the command that does work.
    ///
    /// Returns `None` for [`Self::Managed`], which is updatable.
    pub fn unsupported_reason(
        &self,
        executable: &Path,
        target_version: &str,
    ) -> Option<OrbitError> {
        let remediation = match self {
            Self::Managed { .. } => return None,
            Self::Npm => format!(
                "npm owns this installation; run `npm install -g @orbit-tools/cli@{target_version}` \
                 (or `npx -y @orbit-tools/cli@{target_version}`)"
            ),
            Self::Homebrew => {
                "Homebrew owns this installation; run `brew update && brew upgrade orbit`"
                    .to_string()
            }
            Self::Cargo => format!(
                "cargo owns this installation; run `cargo install --git https://github.com/danieljhkim/orbit --tag v{target_version} --locked orbit-cli`, \
                 or reinstall through the managed installer with `curl -sSf https://raw.githubusercontent.com/danieljhkim/orbit/main/install.sh | sh`"
            ),
            Self::LocalBuild => {
                "this is a local build inside a checkout; update the checkout and rebuild \
                 (`git pull && make install`)"
                    .to_string()
            }
            Self::Unknown => format!(
                "Orbit does not recognize the installer that owns this path; \
                 install through the managed installer with \
                 `curl -sSf https://raw.githubusercontent.com/danieljhkim/orbit/main/install.sh | sh`, \
                 or set {INSTALL_DIR_ENV} to the directory it owns"
            ),
        };
        Some(OrbitError::InvalidInput(format!(
            "cannot update '{}' in place: {remediation}",
            executable.display()
        )))
    }
}

/// The managed install directory for this process: `$ORBIT_INSTALL_DIR`, else
/// `$HOME/.orbit/bin`. Mirrors `install.sh`.
pub fn managed_install_dir() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os(INSTALL_DIR_ENV).filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(explicit));
    }
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| PathBuf::from(home).join(".orbit").join("bin"))
}

/// The release target triple for the platform this binary was built for.
///
/// Derived from the compile-time target rather than probed at runtime: the
/// asset that can replace this executable is the one built for the same
/// triple, and a cross-installed binary reporting its host's `uname` would
/// pick the wrong archive.
pub fn release_target_triple() -> Result<&'static str, OrbitError> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        (os, arch) => Err(OrbitError::InvalidInput(format!(
            "orbit publishes no release archive for {os}/{arch}; \
             supported platforms are Linux x86_64/aarch64 and macOS aarch64/x86_64"
        ))),
    }
}

/// Name of the release archive for `target`.
pub fn release_archive_name(target: &str) -> String {
    format!("orbit-{target}.tar.gz")
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    // Compare canonically where both sides exist, so `~/.orbit/bin` reached
    // through a symlinked HOME still matches the executable's own parent.
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn has_component(path: &Path, name: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str() == name)
}

/// A `cargo build` output lives at `<...>/target/{debug,release}/orbit`, or
/// under `target/<triple>/{debug,release}/orbit` for a cross build.
fn is_cargo_build_output(executable: &Path) -> bool {
    let Some(profile_dir) = executable.parent() else {
        return false;
    };
    let profile_is_cargo = profile_dir
        .file_name()
        .is_some_and(|name| name == "debug" || name == "release");
    profile_is_cargo && has_component(profile_dir, "target")
}
