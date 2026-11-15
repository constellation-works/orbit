//! The probe a granted script root passes twice (script steps §9 item 5):
//! once by `orbit script grant`, in the granting process, and again by the
//! job's process before every spawn. On macOS the two can differ, because
//! privacy permissions (TCC) are attributed to the responsible process (the
//! clock's launch agent or the binary it runs), not to the terminal that
//! granted.

use std::ffi::OsStr;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// How a granted root is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptRootAccess {
    /// A `read_roots` entry: a file or directory the script reads.
    Read,
    /// A `write_roots` entry: a directory the script writes.
    Write,
}

impl ScriptRootAccess {
    fn label(self) -> &'static str {
        match self {
            Self::Read => "read root",
            Self::Write => "write root",
        }
    }
}

/// Check one granted root: it is absolute, exists, canonicalizes, lies
/// outside every credential path, and this process can read it (list a
/// directory, open a file) and, for a write root, create and remove a file
/// in it. Returns the physical path. The error names the path and, on
/// macOS, the privacy pane in which to allow the process that runs the job.
pub fn probe_script_root(
    root: &Path,
    access: ScriptRootAccess,
    home: Option<&OsStr>,
) -> Result<PathBuf, String> {
    let label = access.label();
    if !root.is_absolute() {
        return Err(format!(
            "granted {label} {} is not an absolute path",
            root.display()
        ));
    }
    let physical = root.canonicalize().map_err(|error| match error.kind() {
        ErrorKind::NotFound => format!("granted {label} {} does not exist", root.display()),
        _ => denied(label, root, &error),
    })?;
    if let Some(credential) = super::credential_denies(home)
        .into_iter()
        .map(|(path, _)| path.canonicalize().unwrap_or(path))
        .find(|credential| physical.starts_with(credential))
    {
        return Err(format!(
            "granted {label} {} lies within the credential path {}, which no script reads or \
             writes",
            root.display(),
            credential.display()
        ));
    }
    let metadata = std::fs::metadata(&physical).map_err(|error| denied(label, root, &error))?;
    if metadata.is_dir() {
        let handle = open_root_probe(&physical).map_err(|error| denied(label, root, &error))?;
        let opened = handle
            .metadata()
            .map_err(|error| denied(label, root, &error))?;
        if !opened.is_dir() {
            return Err(format!(
                "granted {label} {} is not a directory",
                root.display()
            ));
        }
        drop(handle);
        std::fs::read_dir(&physical)
            .and_then(|mut entries| entries.next().transpose().map(|_| ()))
            .map_err(|error| denied(label, root, &error))?;
    } else if access == ScriptRootAccess::Write {
        return Err(format!(
            "granted {label} {} is not a directory",
            root.display()
        ));
    } else {
        // The open is the authority, not the metadata above: a regular
        // file replaced by a FIFO (or another non-regular kind) between
        // the two must be refused from the opened handle, and the
        // non-blocking open must not wait for a FIFO writer.
        let handle = open_root_probe(&physical).map_err(|error| denied(label, root, &error))?;
        let opened = handle
            .metadata()
            .map_err(|error| denied(label, root, &error))?;
        if opened.is_dir() {
            return Err(format!(
                "granted {label} {} changed type while it was probed",
                root.display()
            ));
        }
        if !opened.is_file() {
            return Err(format!(
                "granted {label} {} is not a regular file or directory",
                root.display()
            ));
        }
    }
    if access == ScriptRootAccess::Write {
        write_probe(&physical).map_err(|error| denied(label, root, &error))?;
    }
    Ok(physical)
}

/// Open `physical` without blocking on a FIFO and without following a
/// symlink swapped in after canonicalisation. Callers validate the type
/// from the opened handle, so a kind swapped in between the path metadata
/// check and this open is still refused rather than waited on.
fn open_root_probe(physical: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(physical)
    }
    #[cfg(not(unix))]
    {
        std::fs::File::open(physical)
    }
}

/// Create and remove one uniquely named file in `dir`.
fn write_probe(dir: &Path) -> std::io::Result<()> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or_default();
    let probe = dir.join(format!(
        ".orbit-script-root-probe-{}-{nanos:09}",
        std::process::id()
    ));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)?;
    std::fs::remove_file(&probe)
}

fn denied(label: &str, root: &Path, error: &std::io::Error) -> String {
    let mut message = format!(
        "granted {label} {} is not accessible to this process: {error}",
        root.display()
    );
    if cfg!(target_os = "macos") && error.kind() == ErrorKind::PermissionDenied {
        message.push_str(&format!(
            "; if macOS privacy permissions block it, allow the process that runs the job (the \
             Orbit clock's launch agent, or the `orbit` binary it runs) in System Settings > \
             Privacy & Security > {}",
            privacy_pane(root)
        ));
    }
    message
}

/// The System Settings privacy pane that governs `root` for a process.
fn privacy_pane(root: &Path) -> &'static str {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let under_home = |folder: &str| {
        home.as_ref()
            .is_some_and(|home| root.starts_with(home.join(folder)))
    };
    if ["Desktop", "Documents", "Downloads"]
        .iter()
        .any(|folder| under_home(folder))
    {
        "Files and Folders"
    } else if root.starts_with("/Volumes") {
        "Files and Folders (Removable Volumes or Network Volumes)"
    } else {
        "Full Disk Access"
    }
}
