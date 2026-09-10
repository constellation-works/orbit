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
use std::process::Command;

use orbit_common::OrbitError;

/// Environment variable `install.sh` reads for the managed install directory.
pub const INSTALL_DIR_ENV: &str = "ORBIT_INSTALL_DIR";

/// The formula name every current install instruction and diagnostic names.
/// Fully qualified so `brew` never has to guess between it and a retired tap.
pub const CANONICAL_HOMEBREW_FORMULA: &str = "constellation-works/tap/orbit";

/// The formula Orbit published under before consolidating on the
/// `constellation-works` tap. Still resolvable by `brew`, and still what a
/// machine set up before the move has installed — its Cellar keg shares the
/// canonical formula's short name, so the two conflict rather than
/// coexisting.
pub const LEGACY_HOMEBREW_FORMULA: &str = "danieljhkim/tap/orbit";

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
    /// A Homebrew formula owns this binary. `remediation` is resolved once,
    /// against the formulae `brew` reports installed, by
    /// [`Self::detect_with_homebrew_ownership`] — `None` until then, in which
    /// case [`Self::unsupported_reason`] falls back to the plain qualified
    /// upgrade rather than guessing which tap owns it.
    Homebrew {
        /// The exact remediation text, once resolved.
        remediation: Option<String>,
    },
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
            Self::Homebrew { .. } => "homebrew",
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
            return Self::Homebrew { remediation: None };
        }
        if is_cargo_build_output(executable) {
            return Self::LocalBuild;
        }
        if parent.is_some_and(|parent| parent.ends_with(".cargo/bin")) {
            return Self::Cargo;
        }
        Self::Unknown
    }

    /// [`Self::detect`], then — only for a bare Homebrew match — resolve
    /// which formula owns the install by asking `inventory`, so the
    /// remediation names an ordinary qualified upgrade or the legacy-tap
    /// migration instead of leaving it unresolved.
    pub fn detect_with_homebrew_ownership(
        executable: &Path,
        managed_install_dir: Option<&Path>,
        inventory: &dyn HomebrewInventory,
    ) -> Self {
        match Self::detect(executable, managed_install_dir) {
            Self::Homebrew { .. } => Self::Homebrew {
                remediation: Some(homebrew_remediation(inventory.installed_full_names())),
            },
            other => other,
        }
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
            Self::Homebrew { remediation } => remediation.clone().unwrap_or_else(|| {
                format!(
                    "Homebrew owns this installation; run \
                     `brew update && brew upgrade {CANONICAL_HOMEBREW_FORMULA}`"
                )
            }),
            Self::Cargo => format!(
                "cargo owns this installation; run `cargo install --git https://github.com/constellation-works/orbit --tag v{target_version} --locked orbit-cli`, \
                 or reinstall through the managed installer with `curl -sSf https://raw.githubusercontent.com/constellation-works/orbit/main/install.sh | sh`"
            ),
            Self::LocalBuild => {
                "this is a local build inside a checkout; update the checkout and rebuild \
                 (`git pull && make install`)"
                    .to_string()
            }
            Self::Unknown => format!(
                "Orbit does not recognize the installer that owns this path; \
                 install through the managed installer with \
                 `curl -sSf https://raw.githubusercontent.com/constellation-works/orbit/main/install.sh | sh`, \
                 or set {INSTALL_DIR_ENV} to the directory it owns"
            ),
        };
        Some(OrbitError::InvalidInput(format!(
            "cannot update '{}' in place: {remediation}",
            executable.display()
        )))
    }
}

/// Reports which Orbit formula full names Homebrew currently has installed —
/// the signal [`InstallChannel::detect_with_homebrew_ownership`] needs to
/// tell an ordinary canonical upgrade from a legacy-tap migration.
pub trait HomebrewInventory {
    /// Full names (`tap/formula`) `brew list --formula --full-name` reports.
    fn installed_full_names(&self) -> Result<Vec<String>, OrbitError>;
}

/// Asks the real `brew` on the caller's `PATH`.
///
/// Tests construct this directly with `command` pointed at a fake `brew`
/// fixture instead of touching the process's `PATH`.
pub(crate) struct SystemHomebrewInventory {
    pub(crate) command: PathBuf,
}

impl SystemHomebrewInventory {
    /// Probe the real `brew` found on `PATH`.
    pub(crate) fn system() -> Self {
        Self {
            command: PathBuf::from("brew"),
        }
    }
}

impl HomebrewInventory for SystemHomebrewInventory {
    fn installed_full_names(&self) -> Result<Vec<String>, OrbitError> {
        let output = Command::new(&self.command)
            .args(["list", "--formula", "--full-name"])
            .output()
            .map_err(|error| {
                OrbitError::Execution(format!(
                    "failed to run '{} list --formula --full-name': {error}",
                    self.command.display()
                ))
            })?;
        if !output.status.success() {
            return Err(OrbitError::Execution(format!(
                "'{} list --formula --full-name' failed: {}",
                self.command.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect())
    }
}

/// Build the exact Homebrew remediation from the formula full names
/// `inventory` reported, or the diagnostic if it could not be asked.
///
/// An installed [`LEGACY_HOMEBREW_FORMULA`] always wins the recommendation,
/// even alongside the canonical one — Homebrew keys formulae by short name,
/// so having both installed at once is itself the conflict this guides the
/// operator out of. When the listing does not mention either formula, or the
/// probe itself failed, the fallback is still the fully qualified canonical
/// upgrade — never a bare, ambiguous `brew upgrade orbit`.
pub fn homebrew_remediation(inventory: Result<Vec<String>, OrbitError>) -> String {
    let legacy_installed = matches!(
        &inventory,
        Ok(names) if names.iter().any(|name| name == LEGACY_HOMEBREW_FORMULA)
    );
    if legacy_installed {
        return format!(
            "Homebrew owns this installation through the retired `{LEGACY_HOMEBREW_FORMULA}` \
             formula; migrate to the canonical tap without removing unrelated packages or taps: \
             `brew uninstall {LEGACY_HOMEBREW_FORMULA} && brew update && \
             brew install {CANONICAL_HOMEBREW_FORMULA}`"
        );
    }
    match inventory {
        Ok(_) => format!(
            "Homebrew owns this installation; run \
             `brew update && brew upgrade {CANONICAL_HOMEBREW_FORMULA}`"
        ),
        Err(error) => format!(
            "Homebrew owns this installation; run \
             `brew update && brew upgrade {CANONICAL_HOMEBREW_FORMULA}` \
             (could not confirm the installed formula: {error})"
        ),
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
