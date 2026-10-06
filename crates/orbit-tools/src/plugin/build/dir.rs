//! Plugin build directory lifecycle.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;

use super::env::PLUGIN_BUILD_DIR_PREFIX;

/// A build directory, removed when dropped.
#[derive(Debug)]
pub struct PluginBuildDir {
    path: PathBuf,
}

impl PluginBuildDir {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PluginBuildDir {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.path) {
            tracing::warn!(
                target: "orbit.tools.plugin",
                path = %self.path.display(),
                "could not remove the plugin build directory: {error}",
            );
        }
    }
}

/// Whether a `.build-<pid>-<nonce>` directory belongs to a build that is
/// still running. Anything else under the prefix is a leftover.
pub fn is_live_plugin_build_dir(name: &OsStr) -> bool {
    let Some(pid) = name
        .to_str()
        .and_then(|name| name.strip_prefix(PLUGIN_BUILD_DIR_PREFIX))
        .and_then(|rest| rest.split_once('-'))
        .and_then(|(pid, _)| pid.parse::<i32>().ok())
        .filter(|pid| *pid > 0)
    else {
        return false;
    };
    process_is_alive(pid)
}

#[cfg(unix)]
fn process_is_alive(pid: i32) -> bool {
    // SAFETY: signal 0 checks for existence and permission; it sends nothing.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_is_alive(_pid: i32) -> bool {
    false
}

/// `<namespace_dir>/.build-<pid>-<nonce>/` with `src/`, `home/`, `tmp/`,
/// mode 0700, created one component at a time without following a link.
pub(super) fn create_build_dir(namespace_dir: &Path) -> Result<PluginBuildDir, OrbitError> {
    let plugins = namespace_dir.parent().ok_or_else(|| {
        OrbitError::InvalidInput("a build needs a plugin namespace directory".to_string())
    })?;
    let global = plugins.parent().ok_or_else(|| {
        OrbitError::InvalidInput("a build needs a global plugin root".to_string())
    })?;
    let global = global
        .canonicalize()
        .map_err(|error| OrbitError::Io(format!("resolve {}: {error}", global.display())))?;
    let mut namespace = global;
    for component in [plugins.file_name(), namespace_dir.file_name()] {
        let component = component.ok_or_else(|| {
            OrbitError::InvalidInput("a build needs named plugin directories".to_string())
        })?;
        namespace.push(component);
        match std::fs::symlink_metadata(&namespace) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(OrbitError::PolicyDenied(format!(
                    "build parent {} is a link or is not a directory; nothing was built",
                    namespace.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => private_dir(&namespace)?,
            Err(error) => {
                return Err(OrbitError::Io(format!(
                    "stat {}: {error}",
                    namespace.display()
                )));
            }
        }
    }
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce)
        .map_err(|error| OrbitError::Io(format!("build directory nonce: {error}")))?;
    let nonce: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let path = namespace.join(format!(
        "{PLUGIN_BUILD_DIR_PREFIX}{}-{nonce}",
        std::process::id()
    ));
    private_dir(&path)?;
    let dir = PluginBuildDir { path };
    for child in ["src", "home", "tmp"] {
        private_dir(&dir.path.join(child))?;
    }
    Ok(dir)
}

/// `mkdir` that fails on an existing path, so a planted link is never used.
fn private_dir(path: &Path) -> Result<(), OrbitError> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|error| OrbitError::Io(format!("create {}: {error}", path.display())))
}

/// Copy the pristine checkout into the build's `src/`: regular files with
/// their permission bits, directories, and symbolic links as links. `.git`
/// was already removed; anything else (sockets, devices) is skipped.
pub(super) fn copy_checkout(source: &Path, target: &Path) -> Result<(), OrbitError> {
    let io = |what: &str, path: &Path, error: std::io::Error| {
        OrbitError::Io(format!("{what} {}: {error}", path.display()))
    };
    for entry in std::fs::read_dir(source).map_err(|error| io("read", source, error))? {
        let entry = entry.map_err(|error| io("read", source, error))?;
        let path = entry.path();
        let destination = target.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|error| io("stat", &path, error))?;
        if file_type.is_symlink() {
            #[cfg(unix)]
            {
                let link = std::fs::read_link(&path).map_err(|error| io("read", &path, error))?;
                std::os::unix::fs::symlink(link, &destination)
                    .map_err(|error| io("create", &destination, error))?;
            }
        } else if file_type.is_dir() {
            std::fs::create_dir(&destination).map_err(|error| io("create", &destination, error))?;
            copy_checkout(&path, &destination)?;
        } else if file_type.is_file() {
            std::fs::copy(&path, &destination).map_err(|error| io("copy", &path, error))?;
        }
    }
    Ok(())
}
