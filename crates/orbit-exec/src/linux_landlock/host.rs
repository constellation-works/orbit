//! The host paths a confined child may read.
//!
//! An activity's filesystem profile describes the workspace and nothing else,
//! so the host side of the ruleset is decided here. Granting `$HOME` or `/etc`
//! wholesale would hand an agent every credential on the machine, and granting
//! nothing stops a dynamically linked program from starting at all. The tables
//! below are the complete middle ground, in three groups:
//!
//! 1. **Runtime** — what any program needs to execute: the loader, its cache,
//!    the system binaries and libraries, and the character devices a process
//!    opens before it does anything interesting. No user data.
//! 2. **Resolver and trust** — the world-readable files a network client
//!    consults to turn a hostname into an address and to validate a
//!    certificate. No secrets: `/etc/shadow` and the rest of `/etc` are absent,
//!    and `/etc` itself is never granted as a directory.
//! 3. **Tool state** — the directories on the child's own `PATH`, plus one
//!    directory per tool, each named by an environment variable the operator
//!    admitted into the child environment or by that tool's documented default
//!    under `$HOME`. This is what makes `git`, `rg`, `cargo`, `make`, and `gh`
//!    work through a scoped spawn.
//!
//! `$HOME` itself is never granted, and neither is any credential store
//! belonging to a tool that is not on this list — `~/.ssh`, `~/.aws`, and a
//! provider CLI's own state stay unreadable. Consequences worth knowing:
//! SSH-authenticated `git` does not work through a scoped spawn (use HTTPS or
//! `gh`), and a toolchain whose runtime files live outside the `bin` directory
//! on `PATH` needs that tree named by an environment variable.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::{LandlockPathGrant, workspace};

/// System directories a dynamically linked program executes out of.
const RUNTIME_DIRS: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/libx32",
    "/etc/alternatives",
    "/etc/ld.so.conf.d",
    "/dev/pts",
];

/// Loader configuration and the character devices a process opens on startup.
const RUNTIME_FILES: &[&str] = &[
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
    "/dev/ptmx",
];

/// `/proc` entries describing the machine rather than another process.
///
/// `/proc` as a whole is deliberately absent: `/proc/<pid>/environ` of any
/// same-user process would hand the child the parent's credentials, which is
/// exactly the disclosure this confinement exists to prevent. The child's own
/// `/proc/self` is granted after `fork`, where it resolves to the child.
const RUNTIME_PROC_DIRS: &[&str] = &["/proc/sys"];
const RUNTIME_PROC_FILES: &[&str] = &[
    "/proc/cpuinfo",
    "/proc/filesystems",
    "/proc/loadavg",
    "/proc/meminfo",
    "/proc/stat",
    "/proc/uptime",
    "/proc/version",
];

/// Name resolution, service lookup, and time zone data.
const RESOLVER_FILES: &[&str] = &[
    "/etc/gai.conf",
    "/etc/gitconfig",
    "/etc/group",
    "/etc/host.conf",
    "/etc/hosts",
    "/etc/localtime",
    "/etc/nsswitch.conf",
    "/etc/passwd",
    "/etc/protocols",
    "/etc/resolv.conf",
    "/etc/services",
    "/etc/timezone",
];

/// Certificate authority stores.
const TRUST_DIRS: &[&str] = &["/etc/ca-certificates", "/etc/pki", "/etc/ssl"];

/// A tool's state directory: the environment variable that names it, and the
/// tool's documented default when that variable is absent. A relative default
/// is resolved against `$HOME`; an absolute one is used as written.
struct ToolState {
    variable: &'static str,
    default_path: Option<&'static str>,
}

/// Tool state Orbit's own shipped activity allowlists depend on.
///
/// Every entry here is a *specific* directory. Adding one is a security
/// decision: it grants the child read access to that path on the host.
const TOOL_STATE: &[ToolState] = &[
    ToolState {
        variable: "CARGO_HOME",
        default_path: Some(".cargo"),
    },
    ToolState {
        variable: "RUSTUP_HOME",
        default_path: Some(".rustup"),
    },
    ToolState {
        variable: "GIT_CONFIG_GLOBAL",
        default_path: Some(".gitconfig"),
    },
    ToolState {
        variable: "GIT_CONFIG_SYSTEM",
        default_path: None,
    },
    ToolState {
        variable: "GH_CONFIG_DIR",
        default_path: Some(".config/gh"),
    },
    ToolState {
        variable: "SSL_CERT_DIR",
        default_path: None,
    },
    ToolState {
        variable: "SSL_CERT_FILE",
        default_path: None,
    },
    // Toolchains installed outside a system prefix. Each is the variable the
    // tool itself documents, so an operator who installs Node through `nvm` or
    // Python into a virtualenv names that tree the same way they name it for
    // any other consumer.
    ToolState {
        variable: "NVM_DIR",
        default_path: None,
    },
    ToolState {
        variable: "NPM_CONFIG_PREFIX",
        default_path: None,
    },
    ToolState {
        variable: "NODE_PATH",
        default_path: None,
    },
    ToolState {
        variable: "VIRTUAL_ENV",
        default_path: None,
    },
    ToolState {
        variable: "ORBIT_ROOT",
        default_path: None,
    },
    ToolState {
        variable: "ORBIT_REGISTRY_ROOT",
        default_path: None,
    },
    ToolState {
        variable: "ORBIT_BIN",
        default_path: None,
    },
    // A temporary file is opened read-write, so a compiler or patch tool that
    // cannot read its own scratch space fails. There is deliberately no `/tmp`
    // default: a shared temporary directory holds other runs' work, and
    // granting it would undo the outside-workspace boundary for everything
    // staged there. `TMPDIR` is already an admitted baseline variable, so an
    // executor that exports it gets a scratch grant for exactly that path.
    ToolState {
        variable: "TMPDIR",
        default_path: None,
    },
];

/// Git's XDG configuration directory, which has no dedicated variable.
const XDG_TOOL_STATE: &[&str] = &["git"];

/// Credential file names carved back out of a granted tool state directory.
///
/// These are publish and administrative tokens (`$CARGO_HOME/credentials.toml`
/// is a crates.io publish token) that a tool never needs in order to build,
/// inspect, or fetch. The tool's *own* sign-in material — `gh`'s `hosts.yml`,
/// for instance — is deliberately not on this list: an activity that allowlists
/// `gh` has already granted the agent that identity, so withholding the file
/// would remove the tool rather than the capability.
///
/// Only the top level of a granted directory is scanned. These are the
/// documented locations, and recursing a Cargo registry to look for them would
/// cost more than compiling the whole ruleset.
const CREDENTIAL_FILE_NAMES: &[&str] = &["credentials", "credentials.json", "credentials.toml"];

/// Every environment variable that widens the host read set.
///
/// Exposed so an operator can see, and a test can pin, exactly which names in a
/// child environment turn into filesystem grants.
pub const HOST_READ_ENV_VARS: &[&str] = &[
    "CARGO_HOME",
    "GH_CONFIG_DIR",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "NODE_PATH",
    "NPM_CONFIG_PREFIX",
    "NVM_DIR",
    "ORBIT_BIN",
    "ORBIT_REGISTRY_ROOT",
    "ORBIT_ROOT",
    "PATH",
    "RUSTUP_HOME",
    "SSL_CERT_DIR",
    "SSL_CERT_FILE",
    "TMPDIR",
    "VIRTUAL_ENV",
    "XDG_CONFIG_HOME",
];

/// Compile the host half of the ruleset for a child running with
/// `environment`.
pub(super) fn host_read_grants(environment: &[(String, String)]) -> Vec<LandlockPathGrant> {
    let mut grants = Vec::new();
    for dir in RUNTIME_DIRS
        .iter()
        .chain(RUNTIME_PROC_DIRS)
        .chain(TRUST_DIRS)
    {
        push_existing_dir(&mut grants, Path::new(dir));
    }
    for file in RUNTIME_FILES
        .iter()
        .chain(RUNTIME_PROC_FILES)
        .chain(RESOLVER_FILES)
    {
        push_existing_file(&mut grants, Path::new(file));
    }
    let home = lookup(environment, "HOME").map(PathBuf::from);
    for dir in path_directories(environment, home.as_deref()) {
        push_existing_dir(&mut grants, &dir);
    }
    for path in tool_state_paths(environment, home.as_deref()) {
        push_tool_state(&mut grants, &path);
    }
    grants
}

/// The directories the child's `PATH` resolves executables out of.
///
/// A shipped allowlist names programs, not locations: `rg` may live in
/// `~/.local/bin` and `cargo` in `~/.cargo/bin`. `PATH` is the operator's own
/// statement of where those programs are, so it is what the grants follow.
fn path_directories(environment: &[(String, String)], home: Option<&Path>) -> Vec<PathBuf> {
    let Some(search_path) = lookup(environment, "PATH") else {
        return Vec::new();
    };
    std::env::split_paths(search_path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .filter(|dir| is_narrow_enough(dir, home))
        .collect()
}

/// Grant the program itself, in case it lives outside the system directories
/// (a `~/.cargo/bin` shim, or Orbit's own binary under `$ORBIT_BIN`).
pub(super) fn program_grants(
    program: &str,
    environment: &[(String, String)],
) -> Vec<LandlockPathGrant> {
    resolve_program(program, environment)
        .map(|path| vec![LandlockPathGrant::read_file(path)])
        .unwrap_or_default()
}

/// The tool state paths admitted for `environment`, in table order.
fn tool_state_paths(environment: &[(String, String)], home: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for entry in TOOL_STATE {
        let candidate = lookup(environment, entry.variable)
            .map(PathBuf::from)
            .or_else(|| default_path(entry.default_path?, home));
        if let Some(candidate) = candidate {
            paths.push(candidate);
        }
    }
    if let Some(config) = xdg_config_home(environment, home) {
        paths.extend(XDG_TOOL_STATE.iter().map(|tool| config.join(tool)));
    }
    paths
        .into_iter()
        .filter(|path| is_narrow_enough(path, home))
        .collect()
}

/// Resolve a table default: absolute as written, relative against `$HOME`.
fn default_path(default: &str, home: Option<&Path>) -> Option<PathBuf> {
    let path = Path::new(default);
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        Some(home?.join(path))
    }
}

fn xdg_config_home(environment: &[(String, String)], home: Option<&Path>) -> Option<PathBuf> {
    lookup(environment, "XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(home?.join(".config")))
}

/// Reject a tool state path broad enough to defeat the point of the table:
/// the filesystem root, `$HOME`, or any ancestor of `$HOME`.
fn is_narrow_enough(path: &Path, home: Option<&Path>) -> bool {
    if path.parent().is_none() {
        return false;
    }
    match home {
        Some(home) => !home.starts_with(path),
        None => true,
    }
}

/// Grant one tool state path with its credential files carved back out.
///
/// A directory that cannot be walked is skipped rather than granted whole: the
/// carve-out is what makes granting it acceptable in the first place.
fn push_tool_state(grants: &mut Vec<LandlockPathGrant>, path: &Path) {
    let Ok(canonical) = path.canonicalize() else {
        return;
    };
    let credentials = top_level_credential_files(&canonical);
    if let Ok(carved) = workspace::carve_out(&canonical, &credentials) {
        grants.extend(carved);
    }
}

fn top_level_credential_files(dir: &Path) -> BTreeSet<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return BTreeSet::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| CREDENTIAL_FILE_NAMES.contains(&name))
        })
        .filter_map(|entry| entry.path().canonicalize().ok())
        .collect()
}

fn lookup<'a>(environment: &'a [(String, String)], name: &str) -> Option<&'a str> {
    environment
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.is_empty())
}

fn resolve_program(program: &str, environment: &[(String, String)]) -> Option<PathBuf> {
    if program.contains('/') {
        return Path::new(program).canonicalize().ok();
    }
    let search_path = lookup(environment, "PATH")?;
    std::env::split_paths(search_path).find_map(|dir| {
        let candidate = dir.join(program);
        candidate.is_file().then(|| candidate.canonicalize().ok())?
    })
}

fn push_existing_dir(grants: &mut Vec<LandlockPathGrant>, path: &Path) {
    if let Ok(canonical) = path.canonicalize()
        && canonical.is_dir()
    {
        grants.push(LandlockPathGrant::read_tree(canonical));
    }
}

/// Grant one non-directory path. Character devices such as `/dev/null` are not
/// regular files, so the check is "exists and is not a directory" rather than
/// `is_file`.
fn push_existing_file(grants: &mut Vec<LandlockPathGrant>, path: &Path) {
    if let Ok(canonical) = path.canonicalize()
        && !canonical.is_dir()
    {
        grants.push(LandlockPathGrant::read_file(canonical));
    }
}
