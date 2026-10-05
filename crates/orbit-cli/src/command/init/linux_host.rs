//! Explicit Linux host preparation for `orbit init` and the shell installer.
//! Package/profile changes never run during dispatch or npm postinstall.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::{Command, Stdio};

use orbit_core::OrbitError;

use crate::output::sink::stderr_is_terminal;
use orbit_core::bootstrap::linux_sandbox_host::{
    BwrapProbeOutcome, probe_bwrap_fresh, probe_bwrap_fresh_for_user,
};

const PROFILE_SOURCE: &str = "/usr/share/apparmor/extra-profiles/bwrap-userns-restrict";
const PROFILE_TARGET: &str = "/etc/apparmor.d/bwrap-userns-restrict";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageManager {
    Apt,
    Dnf,
    Pacman,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Distribution {
    id: String,
    version: String,
    manager: PackageManager,
    ubuntu_profile: bool,
}

impl Distribution {
    fn detect(release: &str) -> Result<Self, OrbitError> {
        let id = os_release_value(release, "ID").unwrap_or_default();
        let version = os_release_value(release, "VERSION_ID").unwrap_or_default();
        let major = version.split('.').next().unwrap_or_default();
        let (manager, ubuntu_profile) = match (id.as_str(), version.as_str(), major) {
            ("ubuntu", "24.04", _) => (PackageManager::Apt, true),
            ("debian", "13", _) => (PackageManager::Apt, false),
            ("fedora", _, "43" | "44" | "45") => (PackageManager::Dnf, false),
            ("rhel" | "rocky" | "almalinux" | "centos", _, "10") => (PackageManager::Dnf, false),
            ("arch", _, _) => (PackageManager::Pacman, false),
            _ => {
                return Err(OrbitError::Execution(format!(
                    "Linux sandbox preparation does not support distribution {id} {version}; \
                     no package or security-policy changes were made. See docs/runbooks/linux-sandbox.md"
                )));
            }
        };
        Ok(Self {
            id,
            version,
            manager,
            ubuntu_profile,
        })
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
}

struct RealHost {
    authorized: bool,
    probe_user: Option<(u32, u32)>,
}

impl RealHost {
    fn new() -> Result<Self, OrbitError> {
        let probe_user = if unsafe { libc::geteuid() } == 0 {
            let uid = std::env::var("SUDO_UID")
                .ok()
                .and_then(|value| value.parse::<u32>().ok());
            let gid = std::env::var("SUDO_GID")
                .ok()
                .and_then(|value| value.parse::<u32>().ok());
            match (uid, gid) {
                (Some(uid), Some(gid)) if uid != 0 => Some((uid, gid)),
                _ => return Err(OrbitError::Execution(
                    "root installation cannot identify the intended unprivileged Orbit user; run the installer from that account (sudo will authenticate only for package/profile changes), or set ORBIT_SKIP_HOST_PREREQUISITES=1 when an image build or administrator owns the host's sandbox packages"
                        .to_string(),
                )),
            }
        } else {
            None
        };
        Ok(Self {
            authorized: false,
            probe_user,
        })
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
    distro: &Distribution,
    commands: &mut Vec<String>,
) -> Result<(), OrbitError> {
    match distro.manager {
        PackageManager::Apt => {
            let apt = "/usr/bin/apt-get";
            require_command(host, apt)?;
            run_privileged(host, commands, apt, &["update"])?;
            if distro.ubuntu_profile {
                run_privileged(
                    host,
                    commands,
                    apt,
                    &["install", "--yes", "bubblewrap", "apparmor-profiles"],
                )
            } else {
                run_privileged(host, commands, apt, &["install", "--yes", "bubblewrap"])
            }
        }
        PackageManager::Dnf => {
            let dnf = if host.has_command("/usr/bin/dnf5") {
                "/usr/bin/dnf5"
            } else {
                "/usr/bin/dnf"
            };
            require_command(host, dnf)?;
            run_privileged(host, commands, dnf, &["-y", "install", "bubblewrap"])
        }
        PackageManager::Pacman => {
            let pacman = "/usr/bin/pacman";
            require_command(host, pacman)?;
            run_privileged(
                host,
                commands,
                pacman,
                &["-S", "--needed", "--noconfirm", "bubblewrap"],
            )
        }
    }
}

fn require_command(host: &impl Host, path: &str) -> Result<(), OrbitError> {
    if host.has_command(path) {
        Ok(())
    } else {
        Err(OrbitError::Execution(format!(
            "Linux sandbox preparation requires {path} on this distribution"
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

fn prepare_inner(
    host: &mut impl Host,
    non_interactive: bool,
    commands: &mut Vec<String>,
) -> Result<String, OrbitError> {
    let initial = host.probe();
    if initial.available {
        return Ok("ready for the current unprivileged user; no host changes needed".to_string());
    }
    // A package/profile install cannot grant the missing outer authority.
    if namespace_creation_denied(&initial.detail) {
        return Err(OrbitError::Execution(format!(
            "Linux sandbox namespace creation is denied by the kernel or enclosing container: {}; \
             prepare a native host with unprivileged user namespaces enabled",
            initial.detail
        )));
    }
    let distro = Distribution::detect(&host.os_release()?)?;
    let needs_package = initial.detail.contains("not available at")
        || initial
            .detail
            .contains("does not support the required --bind-fd");
    if needs_package {
        host.authorize(non_interactive)?;
        install_packages(host, &distro, commands)?;
    }
    let mut current = host.probe();
    if current.available {
        return Ok(format!(
            "ready for the current unprivileged user on {} {}; Bubblewrap capability probe passed",
            distro.id, distro.version
        ));
    }
    if namespace_creation_denied(&current.detail) {
        return Err(OrbitError::Execution(format!(
            "Linux sandbox namespace creation remains denied by the kernel or enclosing container after package installation: {}",
            current.detail
        )));
    }
    // The packaged AppArmor rule is a remedy for Ubuntu's specific UID-map
    // denial. Other probe failures may be kernel/container policy or a broken
    // binary; installing a profile for them would change host policy without
    // evidence that it can help.
    if distro.ubuntu_profile
        && current
            .detail
            .contains("setting up uid map: Permission denied")
    {
        let mut source = host.root_file(PROFILE_SOURCE)?;
        if source.is_none() && !needs_package {
            host.authorize(non_interactive)?;
            install_packages(host, &distro, commands)?;
            source = host.root_file(PROFILE_SOURCE)?;
            current = host.probe();
            if current.available {
                return Ok(
                    "ready for the current unprivileged user after package installation"
                        .to_string(),
                );
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
        return Err(OrbitError::Execution(format!(
            "Linux sandbox is not ready for the current unprivileged user on {} {}: {}; \
             check package features, AppArmor, and kernel/container user-namespace policy",
            distro.id, distro.version, final_probe.detail
        )));
    }
    Ok(format!(
        "ready for the current unprivileged user on {} {}; Bubblewrap capability probe passed",
        distro.id, distro.version
    ))
}

pub(super) fn prepare(non_interactive: bool) -> Result<String, OrbitError> {
    let mut host = RealHost::new()?;
    prepare_with(&mut host, non_interactive)
}

#[cfg(test)]
#[path = "tests/linux_host.rs"]
mod tests;
