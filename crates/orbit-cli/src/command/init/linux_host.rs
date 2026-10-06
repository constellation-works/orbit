//! Explicit Linux host preparation for `orbit init` and the shell installer.
//! Package/profile changes never run during dispatch or npm postinstall.
//!
//! The distribution's Bubblewrap is preferred. When it is missing or lacks
//! either descriptor-backed bind option after any supported package install, the static Bubblewrap
//! signed into this Orbit release is installed root-owned at
//! [`BUNDLED_BWRAP_PATH`]; executors trust exactly those two paths.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::{Command, Stdio};

use orbit_core::OrbitError;

use crate::output::sink::stderr_is_terminal;
use orbit_core::bootstrap::linux_sandbox_host::{
    BUNDLED_BWRAP_PATH, BUNDLED_BWRAP_VERSION, BwrapProbeOutcome, BwrapSource, probe_bwrap_fresh,
    probe_bwrap_fresh_for_user,
};

const PROFILE_SOURCE: &str = "/usr/share/apparmor/extra-profiles/bwrap-userns-restrict";
const PROFILE_TARGET: &str = "/etc/apparmor.d/bwrap-userns-restrict";
const INSTALL: &str = "/usr/bin/install";
const REMOVE: &str = "/usr/bin/rm";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageManager {
    Apt,
    Dnf,
    Pacman,
    Zypper,
}

impl PackageManager {
    fn for_family(id: &str) -> Option<Self> {
        match id {
            "ubuntu" | "debian" => Some(Self::Apt),
            "fedora" | "rhel" | "rocky" | "almalinux" | "centos" => Some(Self::Dnf),
            "arch" => Some(Self::Pacman),
            "opensuse" | "opensuse-leap" | "opensuse-tumbleweed" | "suse" | "sles" => {
                Some(Self::Zypper)
            }
            _ => None,
        }
    }

    fn command(self, host: &impl Host) -> &'static str {
        match self {
            Self::Apt => "/usr/bin/apt-get",
            Self::Dnf if host.has_command("/usr/bin/dnf5") => "/usr/bin/dnf5",
            Self::Dnf => "/usr/bin/dnf",
            Self::Pacman => "/usr/bin/pacman",
            Self::Zypper => "/usr/bin/zypper",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Distribution {
    id: String,
    version: String,
    /// `None` when no supported package manager is present; only the
    /// bundled Bubblewrap can prepare it.
    manager: Option<PackageManager>,
    ubuntu_profile: bool,
}

impl Distribution {
    fn detect(release: &str, host: &impl Host) -> Self {
        let id = os_release_value(release, "ID").unwrap_or_default();
        let version = os_release_value(release, "VERSION_ID").unwrap_or_default();
        let id_like = os_release_value(release, "ID_LIKE").unwrap_or_default();
        // Prefer the host's family when several managers are installed, but
        // availability decides the install path. Versions never gate support.
        let manager = std::iter::once(id.as_str())
            .chain(id_like.split_whitespace())
            .filter_map(PackageManager::for_family)
            .chain([
                PackageManager::Apt,
                PackageManager::Dnf,
                PackageManager::Pacman,
                PackageManager::Zypper,
            ])
            .find(|manager| host.has_command(manager.command(host)));
        let ubuntu_profile = id == "ubuntu";
        Self {
            id,
            version,
            manager,
            ubuntu_profile,
        }
    }
}

fn os_release_value(release: &str, key: &str) -> Option<String> {
    release.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name == key).then(|| value.trim_matches('"').trim_matches('\'').to_string())
    })
}

/// The small host boundary lets decision tests run without touching the CI host.
trait Host {
    fn os_release(&self) -> Result<String, OrbitError>;
    fn probe(&mut self) -> BwrapProbeOutcome;
    fn root_file(&self, path: &str) -> Result<Option<Vec<u8>>, OrbitError>;
    fn loaded_profiles(&self) -> Result<Option<String>, OrbitError>;
    fn has_command(&self, path: &str) -> bool;
    fn authorize(&mut self, non_interactive: bool) -> Result<(), OrbitError>;
    fn run_privileged(&mut self, path: &str, args: &[&str]) -> Result<(), OrbitError>;
    /// Authenticate this release's bundled Bubblewrap and stage it privately,
    /// returning the staged path and its signed SHA-256.
    fn stage_bundled(&mut self) -> Result<(String, String), OrbitError>;
    /// SHA-256 of an installed file, `None` when it is absent.
    fn file_sha256(&self, path: &str) -> Result<Option<String>, OrbitError>;
}

struct RealHost {
    authorized: bool,
    probe_user: Option<(u32, u32)>,
    /// Kept until preparation ends so the staged file outlives `install`.
    staged: Option<orbit_cmd::update::bundled_bwrap::StagedBwrap>,
}

impl RealHost {
    fn new() -> Result<Self, OrbitError> {
        let probe_user = intended_probe_user(
            unsafe { libc::geteuid() },
            std::env::var("SUDO_UID").ok().as_deref(),
            std::env::var("SUDO_GID").ok().as_deref(),
        )?;
        Ok(Self {
            authorized: false,
            probe_user,
            staged: None,
        })
    }
}

/// Refuse root preparation before any host operation unless sudo identifies
/// the unprivileged account whose namespace capability must be checked.
fn intended_probe_user(
    euid: u32,
    sudo_uid: Option<&str>,
    sudo_gid: Option<&str>,
) -> Result<Option<(u32, u32)>, OrbitError> {
    if euid != 0 {
        return Ok(None);
    }
    let uid = sudo_uid.and_then(|value| value.parse::<u32>().ok());
    let gid = sudo_gid.and_then(|value| value.parse::<u32>().ok());
    match (uid, gid) {
        (Some(uid), Some(gid)) if uid != 0 => Ok(Some((uid, gid))),
        _ => Err(OrbitError::Execution(
            "root installation cannot identify the intended unprivileged Orbit user; run the installer from that account (sudo will authenticate only for package/profile changes), or set ORBIT_SKIP_HOST_PREREQUISITES=1 when an image build or administrator owns the host's sandbox packages"
                .to_string(),
        )),
    }
}

impl Host for RealHost {
    fn os_release(&self) -> Result<String, OrbitError> {
        fs::read_to_string("/etc/os-release")
            .map_err(|error| OrbitError::Execution(format!("read /etc/os-release: {error}")))
    }

    fn probe(&mut self) -> BwrapProbeOutcome {
        match self.probe_user {
            Some((uid, gid)) => probe_bwrap_fresh_for_user(uid, gid),
            None => probe_bwrap_fresh(),
        }
    }

    fn root_file(&self, path: &str) -> Result<Option<Vec<u8>>, OrbitError> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(OrbitError::Execution(format!("inspect {path}: {error}")));
            }
        };
        if !metadata.is_file() || metadata.uid() != 0 || metadata.permissions().mode() & 0o022 != 0
        {
            return Err(OrbitError::Execution(format!(
                "{path} is not a regular root-owned file protected from group/other writes; \
                 refusing to install or load a security profile"
            )));
        }
        fs::read(path)
            .map(Some)
            .map_err(|error| OrbitError::Execution(format!("read {path}: {error}")))
    }

    fn loaded_profiles(&self) -> Result<Option<String>, OrbitError> {
        match fs::read_to_string("/sys/kernel/security/apparmor/profiles") {
            Ok(profiles) => Ok(Some(profiles)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(OrbitError::Execution(format!(
                "read loaded AppArmor profiles: {error}"
            ))),
        }
    }

    fn has_command(&self, path: &str) -> bool {
        fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }

    fn authorize(&mut self, non_interactive: bool) -> Result<(), OrbitError> {
        if self.authorized {
            return Ok(());
        }
        if unsafe { libc::geteuid() } == 0 {
            self.authorized = true;
            return Ok(());
        }
        if !self.has_command("/usr/bin/sudo") {
            return Err(OrbitError::Execution(
                "Linux sandbox preparation needs administrator authority, but /usr/bin/sudo is unavailable; rerun onboarding with authorized elevation"
                    .to_string(),
            ));
        }
        if !non_interactive && !stderr_is_terminal() {
            return Err(OrbitError::Execution(
                "Linux sandbox preparation needs administrator authentication; rerun orbit init from a terminal or grant noninteractive sudo authority"
                    .to_string(),
            ));
        }
        let mut command = Command::new("/usr/bin/sudo");
        if non_interactive {
            command.arg("-n");
        }
        let status = command
            .arg("-v")
            .stdin(Stdio::null())
            .stdout(Stdio::from(std::io::stderr()))
            .status()
            .map_err(|error| {
                OrbitError::Execution(format!("start sudo authentication: {error}"))
            })?;
        if !status.success() {
            return Err(OrbitError::Execution(if non_interactive {
                "Linux sandbox preparation needs passwordless or already-authorized sudo; noninteractive authentication was denied"
                    .to_string()
            } else {
                "Linux sandbox preparation stopped because administrator authentication was declined or denied; no privileged package/profile command ran"
                    .to_string()
            }));
        }
        self.authorized = true;
        Ok(())
    }

    fn run_privileged(&mut self, path: &str, args: &[&str]) -> Result<(), OrbitError> {
        if !self.authorized {
            return Err(OrbitError::Execution(
                "privileged Linux setup command was not authorized".to_string(),
            ));
        }
        let is_root = unsafe { libc::geteuid() } == 0;
        let mut command = if is_root {
            Command::new(path)
        } else {
            let mut sudo = Command::new("/usr/bin/sudo");
            sudo.args(["-n", "--", path]);
            sudo
        };
        let status = command
            .args(args)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .env("LANG", "C")
            .env("DEBIAN_FRONTEND", "noninteractive")
            .stdin(Stdio::null())
            // Package-manager progress must not corrupt init's JSON stdout.
            .stdout(Stdio::from(std::io::stderr()))
            .status()
            .map_err(|error| OrbitError::Execution(format!("start {path}: {error}")))?;
        if !status.success() {
            return Err(OrbitError::Execution(format!(
                "Linux sandbox prerequisite command {path} failed with {status}; \
                 check the package manager or AppArmor error above, then rerun orbit init"
            )));
        }
        Ok(())
    }

    fn stage_bundled(&mut self) -> Result<(String, String), OrbitError> {
        let staged = orbit_cmd::update::bundled_bwrap::stage_bundled_bwrap_for_this_release()?;
        let staged = self.staged.insert(staged);
        Ok((staged.path().display().to_string(), staged.sha256.clone()))
    }

    fn file_sha256(&self, path: &str) -> Result<Option<String>, OrbitError> {
        match fs::read(path) {
            Ok(bytes) => Ok(Some(orbit_common::security::release::sha256_hex(&bytes))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(OrbitError::Execution(format!("read {path}: {error}"))),
        }
    }
}

fn run_privileged(
    host: &mut impl Host,
    commands: &mut Vec<String>,
    path: &str,
    args: &[&str],
) -> Result<(), OrbitError> {
    commands.push(format!("{path} {}", args.join(" ")));
    host.run_privileged(path, args)
}

fn install_packages(
    host: &mut impl Host,
    manager: PackageManager,
    commands: &mut Vec<String>,
) -> Result<(), OrbitError> {
    let command = manager.command(host);
    require_command(host, command)?;
    match manager {
        PackageManager::Apt => {
            run_privileged(host, commands, command, &["update"])?;
            run_privileged(host, commands, command, &["install", "--yes", "bubblewrap"])
        }
        PackageManager::Dnf => {
            run_privileged(host, commands, command, &["-y", "install", "bubblewrap"])
        }
        PackageManager::Pacman => run_privileged(
            host,
            commands,
            command,
            &["-S", "--needed", "--noconfirm", "bubblewrap"],
        ),
        PackageManager::Zypper => run_privileged(
            host,
            commands,
            command,
            &["--non-interactive", "install", "bubblewrap"],
        ),
    }
}

fn require_command(host: &impl Host, path: &str) -> Result<(), OrbitError> {
    if host.has_command(path) {
        Ok(())
    } else {
        Err(OrbitError::Execution(format!(
            "Linux sandbox preparation requires {path}, but that command was not found"
        )))
    }
}

/// Bubblewrap's diagnostics when the kernel or an enclosing namespace refuses
/// a new user namespace outright: `EPERM` for unprivileged namespaces turned
/// off, and any other `unshare` failure (`EINVAL` without kernel support,
/// `ENOSPC` when a container caps `max_user_namespaces`). Ubuntu's AppArmor
/// restriction instead lets the namespace exist and fails at the UID map.
fn namespace_creation_denied(detail: &str) -> bool {
    detail.contains("No permissions to create new namespace")
        || detail.contains("Creating new namespace failed")
}

fn probe_error(detail: &str) -> OrbitError {
    if namespace_creation_denied(detail) {
        OrbitError::Execution(format!(
            "Linux sandbox namespace creation is denied by the kernel or enclosing container: {detail}; \
             prepare a native host with unprivileged user namespaces enabled"
        ))
    } else {
        OrbitError::Execution(format!(
            "Linux sandbox is not ready for the current unprivileged user: {detail}; \
             check package features, AppArmor, and kernel/container user-namespace policy"
        ))
    }
}

fn ubuntu_uid_map_denied(distro: &Distribution, detail: &str) -> bool {
    distro.ubuntu_profile && detail.contains("setting up uid map: Permission denied")
}

fn profile_is_loaded(profiles: &str) -> bool {
    profiles.lines().any(|line| line.starts_with("bwrap ("))
}

fn prepare_with(host: &mut impl Host, non_interactive: bool) -> Result<String, OrbitError> {
    let mut commands = Vec::new();
    prepare_inner(host, non_interactive, &mut commands).map_err(|error| {
        if commands.is_empty() {
            error
        } else {
            OrbitError::Execution(format!(
                "{error}; privileged commands attempted (host changes may be partial): {}",
                commands.join("; ")
            ))
        }
    })
}

/// The probe found no wrapper it could use: the host's is missing or lacks
/// a required descriptor-backed bind option, and no capable bundled binary is installed. A capable wrapper
/// is the remedy; every other failure is host policy.
fn needs_capable_wrapper(detail: &str) -> bool {
    detail.contains("not available at")
        || detail.contains("does not support the required --bind-fd")
}

/// A probe that passed on a bundled binary older than this release's pin.
fn stale_bundled(probe: &BwrapProbeOutcome) -> Option<&str> {
    (probe.source == Some(BwrapSource::Bundled))
        .then(|| probe.version.as_deref().unwrap_or("an unknown version"))
        .filter(|version| *version != BUNDLED_BWRAP_VERSION)
}

/// Which wrapper a passing probe ran, for the readiness reason.
fn wrapper_in_use(probe: &BwrapProbeOutcome) -> String {
    let source = probe.source.map_or("host", BwrapSource::as_str);
    match &probe.version {
        Some(version) => format!("{source} Bubblewrap {version} at {}", probe.trusted_path),
        None => format!("{source} Bubblewrap at {}", probe.trusted_path),
    }
}

/// Authenticate this release's bundled Bubblewrap, then install it root-owned
/// at its fixed path. Verification runs before any sudo prompt, and the
/// installed bytes are re-hashed so nothing that changed the staged file
/// after verification can be left in place.
fn install_bundled(
    host: &mut impl Host,
    non_interactive: bool,
    commands: &mut Vec<String>,
) -> Result<(), OrbitError> {
    let (staged, sha256) = host.stage_bundled().map_err(|error| {
        OrbitError::Execution(format!(
            "no capable Bubblewrap is installed and the bundled one cannot be used: {error}"
        ))
    })?;
    host.authorize(non_interactive)?;
    require_command(host, INSTALL)?;
    let directory = std::path::Path::new(BUNDLED_BWRAP_PATH)
        .parent()
        .and_then(std::path::Path::to_str)
        .unwrap_or("/");
    run_privileged(
        host,
        commands,
        INSTALL,
        &["-d", "-o", "root", "-g", "root", "-m", "0755", directory],
    )?;
    run_privileged(
        host,
        commands,
        INSTALL,
        &[
            "-o",
            "root",
            "-g",
            "root",
            "-m",
            "0755",
            &staged,
            BUNDLED_BWRAP_PATH,
        ],
    )?;
    if host.file_sha256(BUNDLED_BWRAP_PATH)?.as_deref() == Some(sha256.as_str()) {
        return Ok(());
    }
    require_command(host, REMOVE)?;
    run_privileged(host, commands, REMOVE, &["-f", BUNDLED_BWRAP_PATH])?;
    Err(OrbitError::Execution(format!(
        "the installed {BUNDLED_BWRAP_PATH} does not match the signed release digest {sha256}; removed it"
    )))
}

fn prepare_inner(
    host: &mut impl Host,
    non_interactive: bool,
    commands: &mut Vec<String>,
) -> Result<String, OrbitError> {
    let initial = host.probe();
    if initial.available {
        if let Some(stale) = stale_bundled(&initial).map(str::to_string) {
            install_bundled(host, non_interactive, commands).map_err(|error| {
                OrbitError::Execution(format!(
                    "the sandbox works with bundled Bubblewrap {stale}, but refreshing it to \
                     {BUNDLED_BWRAP_VERSION} failed: {error}"
                ))
            })?;
            let refreshed = host.probe();
            if !refreshed.available {
                return Err(OrbitError::Execution(format!(
                    "the refreshed bundled Bubblewrap fails the capability probe: {}",
                    refreshed.detail
                )));
            }
            return Ok(format!(
                "ready for the current unprivileged user with the {}; refreshed from {stale}",
                wrapper_in_use(&refreshed)
            ));
        }
        return Ok(format!(
            "ready for the current unprivileged user with the {}; no host changes needed",
            wrapper_in_use(&initial)
        ));
    }
    // A package/profile install cannot grant the missing outer authority.
    if namespace_creation_denied(&initial.detail) {
        return Err(probe_error(&initial.detail));
    }
    let distro = Distribution::detect(&host.os_release()?, host);
    let needs_package = needs_capable_wrapper(&initial.detail);
    if needs_package {
        let Some(manager) = distro.manager else {
            return prepare_bundled(host, &distro, non_interactive, commands, &initial.detail);
        };
        host.authorize(non_interactive)?;
        install_packages(host, manager, commands)?;
    }
    let mut current = host.probe();
    if current.available {
        return Ok(format!(
            "ready for the current unprivileged user on {} {} with the {}; Bubblewrap capability probe passed",
            distro.id,
            distro.version,
            wrapper_in_use(&current)
        ));
    }
    if namespace_creation_denied(&current.detail) {
        return Err(probe_error(&current.detail));
    }
    if needs_capable_wrapper(&current.detail) {
        return prepare_bundled(host, &distro, non_interactive, commands, &current.detail);
    }
    // The packaged AppArmor rule is a remedy for Ubuntu's specific UID-map
    // denial. Other probe failures may be kernel/container policy or a broken
    // binary; installing a profile for them would change host policy without
    // evidence that it can help.
    if ubuntu_uid_map_denied(&distro, &current.detail) {
        let mut source = host.root_file(PROFILE_SOURCE)?;
        if source.is_none() {
            host.authorize(non_interactive)?;
            let apt = "/usr/bin/apt-get";
            require_command(host, apt)?;
            run_privileged(host, commands, apt, &["update"])?;
            run_privileged(
                host,
                commands,
                apt,
                &["install", "--yes", "apparmor-profiles"],
            )?;
            source = host.root_file(PROFILE_SOURCE)?;
            current = host.probe();
            if current.available {
                return Ok(
                    "ready for the current unprivileged user after package installation"
                        .to_string(),
                );
            }
            // Installing the package may have changed the failure. Never
            // load a profile unless the fresh probe still has its signature.
            if !ubuntu_uid_map_denied(&distro, &current.detail) {
                return Err(probe_error(&current.detail));
            }
        }
        let source = source.ok_or_else(|| OrbitError::Execution(format!(
            "Ubuntu package did not provide {PROFILE_SOURCE}; cannot install a verified narrow AppArmor profile"
        )))?;
        let loaded = host.loaded_profiles()?.ok_or_else(|| {
            OrbitError::Execution(format!(
                "AppArmor profile registry is unavailable and Bubblewrap still fails: {}",
                current.detail
            ))
        })?;
        if profile_is_loaded(&loaded) {
            return Err(OrbitError::Execution(format!(
                "an AppArmor bwrap profile is already loaded but the unprivileged probe fails: {}; \
                 inspect the existing policy rather than replacing it",
                current.detail
            )));
        }
        let installed = host.root_file(PROFILE_TARGET)?;
        if installed.as_ref().is_some_and(|bytes| bytes != &source) {
            return Err(OrbitError::Execution(format!(
                "existing {PROFILE_TARGET} differs from the packaged profile; refusing to overwrite a custom AppArmor rule"
            )));
        }
        host.authorize(non_interactive)?;
        require_command(host, "/usr/sbin/apparmor_parser")?;
        if installed.is_none() {
            require_command(host, "/usr/bin/install")?;
            run_privileged(
                host,
                commands,
                "/usr/bin/install",
                &["-m", "0644", PROFILE_SOURCE, PROFILE_TARGET],
            )?;
        }
        run_privileged(
            host,
            commands,
            "/usr/sbin/apparmor_parser",
            &["-r", PROFILE_TARGET],
        )?;
        let loaded = host.loaded_profiles()?.unwrap_or_default();
        if !profile_is_loaded(&loaded) {
            return Err(OrbitError::Execution(
                "AppArmor did not report the packaged bwrap profile as loaded".to_string(),
            ));
        }
    }
    let final_probe = host.probe();
    if !final_probe.available {
        return Err(probe_error(&final_probe.detail));
    }
    Ok(format!(
        "ready for the current unprivileged user on {} {}; Bubblewrap capability probe passed",
        distro.id, distro.version
    ))
}

fn prepare_bundled(
    host: &mut impl Host,
    distro: &Distribution,
    non_interactive: bool,
    commands: &mut Vec<String>,
    gap: &str,
) -> Result<String, OrbitError> {
    install_bundled(host, non_interactive, commands).map_err(|error| {
        let manager_gap = if distro.manager.is_none() {
            "no supported package manager was found (apt-get, dnf5/dnf, pacman or zypper); "
        } else {
            ""
        };
        OrbitError::Execution(format!("{gap}; {manager_gap}{error}"))
    })?;
    ready_after_bundled_install(host, distro)
}

fn ready_after_bundled_install(
    host: &mut impl Host,
    distro: &Distribution,
) -> Result<String, OrbitError> {
    let probe = host.probe();
    if probe.available {
        return Ok(format!(
            "ready for the current unprivileged user on {} {} with the {}; installed from this Orbit release",
            distro.id,
            distro.version,
            wrapper_in_use(&probe)
        ));
    }
    Err(OrbitError::Execution(format!(
        "Linux sandbox is not ready for the current unprivileged user on {} {} after installing the bundled Bubblewrap: {}; \
         check kernel/container user-namespace policy",
        distro.id, distro.version, probe.detail
    )))
}

pub(super) fn prepare(non_interactive: bool) -> Result<String, OrbitError> {
    let mut host = RealHost::new()?;
    prepare_with(&mut host, non_interactive)
}

#[cfg(test)]
#[path = "tests/linux_host.rs"]
mod tests;
