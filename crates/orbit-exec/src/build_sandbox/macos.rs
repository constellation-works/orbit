//! `macos-sandbox-build-v1`: a deny-default SBPL profile (§3.2).
//!
//! Not a variant of [`crate::compile_macos_sandbox_profile`], which starts
//! from broad read access for an agent CLI. This one starts from
//! `(deny default)` and allows the system runtime, the consented programs and
//! toolchain roots, and the build directory. It never allows network: macOS
//! runs no `fetch` phase ([`super::BUILD_FETCH_PHASE_SUPPORTED`]).

use std::path::Path;

use super::BuildSandboxSpec;
use crate::macos_sandbox::sbpl_filter::sbpl_escape;

/// System trees a macOS program executes out of: binaries, frameworks, the
/// dyld shared cache, and the time zone database. No user data.
const MACOS_RUNTIME_SUBPATHS: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/System",
    "/Library/Apple",
    "/private/var/db/dyld",
    "/private/var/db/timezone",
    "/private/etc/ssl",
];

/// Resolver, account and loader files, plus the devices a process opens.
const MACOS_RUNTIME_LITERALS: &[&str] = &[
    "/private/etc/hosts",
    "/private/etc/resolv.conf",
    "/private/etc/services",
    "/private/etc/protocols",
    "/private/etc/passwd",
    "/private/etc/group",
    "/private/etc/localtime",
    "/dev/null",
    "/dev/zero",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
    "/dev/dtracehelper",
];

/// The writes every process makes to its terminal and the null device.
pub(super) const DEV_WRITE_ALLOW: &str =
    "(allow file-write-data (literal \"/dev/null\") (literal \"/dev/tty\"))\n";

/// The start of a deny-default profile: what any process needs to start
/// and run, and no file content or network.
pub(super) fn deny_default_preamble() -> String {
    let mut profile = String::from("(version 1)\n(deny default)\n");
    // `stat` on any path: path resolution walks every ancestor of the build
    // directory and the toolchain roots. Metadata is not content; listing a
    // directory or reading a file still needs the allows below.
    profile.push_str("(allow file-read-metadata)\n");
    // macOS 26 process startup reads the root directory itself: without it
    // every exec, even `/usr/bin/true`, aborts with SIGABRT before `main`
    // (kernel log: `deny(1) file-read-data /`). `literal` grants `/` only —
    // its entry names — never a file or directory below it.
    profile.push_str("(allow file-read-data (literal \"/\"))\n");
    profile.push_str("(allow process-fork)\n");
    profile.push_str("(allow process-info* (target same-sandbox))\n");
    profile.push_str("(allow signal (target same-sandbox))\n");
    profile.push_str("(allow sysctl-read)\n");
    // Process startup on current macOS makes these provenance-related MAC
    // syscalls, and a deny-default profile without them fails the launch with
    // a bare `Operation not permitted`. The agent profile carries the same
    // pair (`compile_macos_sandbox_profile`). Neither reads a file or opens
    // the network.
    profile.push_str("(allow system-mac-syscall (mac-policy-name \"vnguard\"))\n");
    profile.push_str(
        "(allow system-mac-syscall (require-all (mac-policy-name \"Sandbox\") (mac-syscall-number 67)))\n",
    );
    profile.push_str(
        "(allow mach-lookup (global-name \"com.apple.system.opendirectoryd.libinfo\"))\n",
    );
    profile
}

/// The system runtime as read filters. The tables are already macOS
/// physical paths, so they render the same on every host that compiles the
/// profile.
pub(super) fn runtime_read_filters() -> Vec<String> {
    let mut readable: Vec<String> = MACOS_RUNTIME_SUBPATHS
        .iter()
        .map(|path| format!("(subpath \"{}\")", sbpl_escape(path)))
        .collect();
    readable.extend(
        MACOS_RUNTIME_LITERALS
            .iter()
            .map(|path| format!("(literal \"{}\")", sbpl_escape(path))),
    );
    readable
}

/// The SBPL profile for an offline phase. `spec.network` is not rendered:
/// `command` refuses a networked phase before compiling.
pub fn compile_macos_build_profile(spec: &BuildSandboxSpec<'_>) -> String {
    let mut profile = deny_default_preamble();
    let mut readable = runtime_read_filters();
    readable.extend(spec.readable.iter().map(|path| subpath(path)));
    let build_dir = subpath(spec.build_dir);
    readable.push(build_dir.clone());
    let readable = readable.join(" ");
    profile.push_str(&format!("(allow file-read* {readable})\n"));
    profile.push_str(&format!("(allow process-exec {readable})\n"));
    profile.push_str(&format!("(allow file-write* {build_dir})\n"));
    profile.push_str(DEV_WRITE_ALLOW);

    // Last-match-wins: the credential denies sit after every allow.
    push_credential_denies(&mut profile, spec.home);

    profile.push_str("(deny network*)\n");
    profile
}

/// The credential denies, read and write. They must follow every allow:
/// SBPL's last match wins.
pub(super) fn push_credential_denies(profile: &mut String, home: Option<&std::ffi::OsStr>) {
    for (path, file) in super::credential_denies(home) {
        let filter = if file {
            format!("(literal \"{}\")", sbpl_escape(&physical(&path)))
        } else {
            subpath(&path)
        };
        profile.push_str(&format!("(deny file-read* file-write* {filter})\n"));
    }
}

pub(super) fn subpath(path: &Path) -> String {
    format!("(subpath \"{}\")", sbpl_escape(&physical(path)))
}

/// Seatbelt matches the kernel's physical path (`/private/var`, not `/var`).
pub(super) fn physical(path: &Path) -> String {
    crate::physical_with_missing_tail(path)
        .display()
        .to_string()
}

#[cfg(target_os = "macos")]
pub(super) fn probe(fetch: bool) -> Result<super::BuildSandboxProbe, String> {
    if fetch {
        return Err(FETCH_UNSUPPORTED.to_string());
    }
    if !crate::sandbox_exec_available() {
        return Err(format!(
            "the {} build profile needs {}",
            orbit_types::plugin::PLUGIN_BUILD_PROFILE_MACOS,
            crate::sandbox_exec_unavailable_message()
        ));
    }
    Ok(super::BuildSandboxProbe {
        profile: orbit_types::plugin::PLUGIN_BUILD_PROFILE_MACOS,
        landlock_abi: None,
    })
}

#[cfg(target_os = "macos")]
const FETCH_UNSUPPORTED: &str = "macOS runs no network `fetch` phase for a plugin build, because a fetch descendant could \
     outlive the phase with its network";

/// The `sandbox-exec` command for one offline phase. The profile is passed
/// inline: a file in the shared temp root could be rewritten by another
/// confined process before `sandbox-exec` reads it.
#[cfg(target_os = "macos")]
pub(super) fn command(
    request: &super::BuildPhaseRequest<'_>,
) -> Result<std::process::Command, orbit_common::OrbitError> {
    use orbit_common::OrbitError;

    if request.sandbox.network != super::BuildPhaseNetwork::None {
        return Err(OrbitError::PluginBuildFetchUnsupported(format!(
            "{FETCH_UNSUPPORTED}; nothing was run"
        )));
    }
    let sandbox_exec = crate::sandbox_exec_path()
        .ok_or_else(|| OrbitError::PolicyDenied(crate::sandbox_exec_unavailable_message()))?;
    let mut command = std::process::Command::new(sandbox_exec);
    command
        .arg("-p")
        .arg(compile_macos_build_profile(&request.sandbox))
        .args(request.argv)
        .current_dir(request.cwd);
    Ok(command)
}
